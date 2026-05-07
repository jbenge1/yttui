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

    /// Register a pid and return a [`RegistrationGuard`] that clears
    /// the registry on drop. Use this instead of [`Self::register`] +
    /// manual [`Self::clear`] to ensure the clear happens on every
    /// exit path of the surrounding scope (#71). The residual window
    /// between an upstream `wait()` returning and Drop running is
    /// bounded to the stack-unwind of the enclosing function — pidfd
    /// would close it entirely, but isn't portable.
    pub fn register_pid_with_guard(&self, pid: u32) -> RegistrationGuard<'_> {
        self.register(pid);
        RegistrationGuard { registry: self }
    }

    /// Spawn-and-register inside a single critical section so a
    /// concurrent signal-watcher cannot observe a transient `None`
    /// while the child is alive (#70). The registry mutex is held for
    /// the entire duration of `spawn`, then the returned pid is
    /// written before the lock is dropped. Returns the spawn closure's
    /// non-pid output (typically the child handle) plus a
    /// [`RegistrationGuard`] that clears on drop.
    ///
    /// # Errors
    /// Propagates whatever error type `spawn` returns.
    ///
    /// # Panics
    /// Panics if the inner mutex was poisoned. See [`Self::register`].
    pub fn register_spawn<C, E, F>(
        &self,
        spawn: F,
    ) -> Result<(C, RegistrationGuard<'_>), E>
    where
        F: FnOnce() -> Result<(C, u32), E>,
    {
        let mut g = self.inner.lock().expect("ChildRegistry mutex poisoned");
        let (child, pid) = spawn()?;
        *g = Some(pid);
        drop(g);
        Ok((child, RegistrationGuard { registry: self }))
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
        // `killpg` is `safe fn` in `nix`, no fork+exec, no /bin/kill
        // dependency, no PATH lookup. Project lints `unsafe_code =
        // forbid` so the syscall stays behind nix's safe wrapper.
        let pid_i = i32::try_from(pid).unwrap_or(i32::MAX);
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid_i),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    #[cfg(not(unix))]
    pub fn kill_group(&self) {
        // Non-Unix: process groups don't exist the same way; the bin
        // itself isn't supported on non-Unix targets at the moment.
    }
}

/// RAII guard that clears the registry when dropped.
///
/// Returned by [`ChildRegistry::register_pid_with_guard`] and
/// [`ChildRegistry::register_spawn`]. Drop runs on every code path
/// (early-return via `?`, panic, normal fallthrough) so the pid is
/// always deregistered. Closes the worst of the reap-vs-clear race
/// (#71); the residual window between `wait()` returning and Drop is
/// bounded to a handful of stack-unwind instructions.
#[derive(Debug)]
pub struct RegistrationGuard<'a> {
    registry: &'a ChildRegistry,
}

impl Drop for RegistrationGuard<'_> {
    fn drop(&mut self) {
        self.registry.clear();
    }
}

/// Install a SIGINT/SIGTERM watcher thread. On the **first** signal:
///   1. Kill the registered child process group (if any).
///   2. Run `cleanup` (typically: `disable_raw_mode` + leave alt screen).
///   3. Exit the process with status 130 (128 + SIGINT) so shells see
///      "interrupted by user", consistent with most CLI tools.
///
/// On a **second** signal arriving while the first-signal cleanup is
/// still in progress, exit immediately with 130 (#72). Matches POSIX
/// shell convention ("hit Ctrl-C twice to escape"); the watcher must
/// not be harder to escape than the system shell when its own kill /
/// cleanup wedges on something pathological.
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
        let mut iter = signals.forever();
        run_watcher_loop(
            || iter.next().map(|_| ()),
            move || {
                registry.kill_group();
                cleanup();
                std::process::exit(130);
            },
            || std::process::exit(130),
        );
    });
    Ok(())
}

/// The watcher's signal-dispatch loop, factored out of
/// [`install_handler`] so the escalation contract (#72) is unit-
/// testable without real signals.
///
/// Contract:
///   - First `next_signal()` → `Some(())`: spawn `graceful` on a side
///     thread (it kills the child and runs cleanup, then process-exits).
///   - Second `next_signal()` → `Some(())` while graceful is still
///     running: call `hard_exit` immediately (skip cleanup; user hit
///     Ctrl-C twice).
///   - Either `next_signal()` → `None`: return without doing anything
///     (channel closed; nothing more to do).
///
/// `graceful` is `FnOnce + Send + 'static` because it's spawned to a
/// side thread; `hard_exit` is `FnOnce` and called in this thread.
fn run_watcher_loop<N, G, H>(
    mut next_signal: N,
    graceful: G,
    hard_exit: H,
) where
    N: FnMut() -> Option<()>,
    G: FnOnce() + Send + 'static,
    H: FnOnce(),
{
    if next_signal().is_none() {
        return;
    }
    std::thread::spawn(graceful);
    if next_signal().is_some() {
        hard_exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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

    #[cfg(unix)]
    #[test]
    fn kill_group_terminates_a_real_child_process_group() {
        // Pin #76: kill_group must actually deliver SIGTERM to the
        // registered process group. Spawn a sleep in its own group,
        // register its pgid, kill_group, then assert wait() returns
        // signal-15 termination. Independent of whether the impl uses
        // /bin/kill or nix::killpg — the contract is "child dies".
        use command_group::CommandGroup;
        use std::os::unix::process::ExitStatusExt;
        use std::process::Command;

        let mut child = Command::new("sleep")
            .arg("30")
            .group_spawn()
            .expect("spawn sleep");
        let r = ChildRegistry::new();
        r.register(child.id());
        r.kill_group();
        let status = child.wait().expect("wait sleep");
        assert_eq!(
            status.signal(),
            Some(libc_sigterm()),
            "expected SIGTERM termination, got {status:?}"
        );
    }

    #[cfg(unix)]
    fn libc_sigterm() -> i32 {
        // Avoid pulling libc in just for the constant; SIGTERM is 15
        // on every Unix yttui targets.
        15
    }

    #[test]
    fn register_spawn_blocks_concurrent_observers_until_pid_is_set() {
        // Pin #70: the spawn-vs-register race. Without this fix, a
        // SIGINT arriving between `Command::spawn()` returning and the
        // registry getting updated would observe `None` and the
        // watcher would skip kill_group, orphaning the child. With
        // `register_spawn`, the registry mutex is held for the entire
        // spawn-and-register critical section, so any concurrent
        // observer of `current()` either sees `None` (spawn hasn't
        // started yet) or `Some(pid)` (spawn done + registered) — never
        // a transient `None` while the child is alive.
        //
        // Deterministic test: a barrier-fenced "spawn" lets us pin the
        // helper inside the lock window, fire an observer thread that
        // contends for the lock, then release the spawn. The observer
        // must NOT see `None` once the spawn function has been called
        // with a non-zero pid.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        let r: &'static ChildRegistry = Box::leak(Box::new(ChildRegistry::new()));
        let spawn_started = Arc::new(AtomicBool::new(false));
        let release_spawn = Arc::new(AtomicBool::new(false));

        let spawn_started_t = spawn_started.clone();
        let release_spawn_t = release_spawn.clone();
        // Channel passes the guard out so the producer thread keeps
        // it alive while the observer reads the registry. Without this
        // the guard's Drop would clear the pid the moment the
        // register_spawn return value is bound to `_`.
        let (guard_tx, guard_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            let ((), guard) = r
                .register_spawn::<(), (), _>(|| {
                    spawn_started_t.store(true, Ordering::SeqCst);
                    while !release_spawn_t.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(((), 424_242))
                })
                .expect("register_spawn ok");
            guard_tx.send(()).unwrap();
            // Hold the guard until the observer has read.
            std::thread::sleep(Duration::from_millis(100));
            drop(guard);
        });

        // Wait for producer to be inside the spawn closure (lock held).
        let deadline = Instant::now() + Duration::from_secs(2);
        while !spawn_started.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "producer never entered spawn");
            std::thread::sleep(Duration::from_millis(1));
        }

        // Concurrent observer: must block on the registry mutex until
        // producer releases. Thread it so we can confirm it didn't
        // return `None` mid-window.
        let observer_pid = Arc::new(std::sync::Mutex::new(None));
        let observer_pid_t = observer_pid.clone();
        let observer = std::thread::spawn(move || {
            let pid = r.current();
            *observer_pid_t.lock().unwrap() = Some(pid);
        });

        // Give the observer time to attempt to lock — it must be
        // blocked since producer holds the lock.
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            observer_pid.lock().unwrap().is_none(),
            "observer should be blocked on the registry lock; it returned early"
        );

        // Release the spawn → producer writes pid → drops lock →
        // observer acquires → reads Some(pid).
        release_spawn.store(true, Ordering::SeqCst);
        // Wait for producer to confirm it acquired the guard (i.e.
        // pid is registered) before joining the observer.
        guard_rx.recv().unwrap();
        observer.join().unwrap();

        let seen_by_observer = observer_pid
            .lock()
            .unwrap()
            .expect("observer thread must have run");
        assert_eq!(
            seen_by_observer,
            Some(424_242),
            "observer must see the registered pid, never the transient None"
        );

        producer.join().unwrap();

        // Cleanup so the static doesn't pollute later tests.
        r.clear();
    }

    #[test]
    fn registration_guard_clears_registry_on_drop() {
        // Pin #71: the reap-vs-clear pid-recycle race. The guard
        // pattern ensures the registry is cleared as part of unwinding
        // the spawn helper's stack frame, rather than as a separate
        // statement at the call site (where ordering can drift). The
        // residual window between `child.wait()` returning and the
        // guard's Drop running is bounded to a few instructions of
        // stack unwind under the lock.
        let r = ChildRegistry::new();
        {
            let _guard = r.register_pid_with_guard(99);
            assert_eq!(r.current(), Some(99));
        }
        assert_eq!(
            r.current(),
            None,
            "guard must clear the registry on drop"
        );
    }

    #[test]
    fn watcher_first_signal_runs_graceful_does_not_hard_exit() {
        // Pin #72 (half 1): exactly one signal → run graceful path,
        // never invoke the hard-exit escape hatch. `next_signal`
        // returns Some once and None thereafter, modelling "user hit
        // Ctrl-C exactly once and let cleanup finish."
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        let graceful_ran = Arc::new(AtomicBool::new(false));
        let hard_ran = Arc::new(AtomicBool::new(false));
        let calls = AtomicU32::new(0);

        let g = graceful_ran.clone();
        let h = hard_ran.clone();

        run_watcher_loop(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 { Some(()) } else { None }
            },
            move || {
                g.store(true, Ordering::SeqCst);
            },
            move || {
                h.store(true, Ordering::SeqCst);
            },
        );

        // Wait briefly for the spawned graceful thread to run.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(1);
        while !graceful_ran.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "graceful never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            !hard_ran.load(Ordering::SeqCst),
            "hard_exit must NOT run on a single signal"
        );
    }

    #[test]
    fn watcher_second_signal_invokes_hard_exit() {
        // Pin #72 (half 2): if the iterator yields twice — i.e. the
        // user hit Ctrl-C while graceful was still running — the loop
        // must call `hard_exit`. This is the POSIX-conventional double-
        // Ctrl-C escalation that the previous one-shot
        // signals.forever().next() could not provide.
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        let hard_ran = Arc::new(AtomicBool::new(false));
        let calls = AtomicU32::new(0);

        let h = hard_ran.clone();

        run_watcher_loop(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 { Some(()) } else { None }
            },
            // Side-thread graceful: simulate "wedged cleanup" by
            // sleeping; the hard-exit must beat us. We don't observe
            // its completion — the contract only demands hard_exit
            // fired.
            || std::thread::sleep(std::time::Duration::from_millis(50)),
            move || h.store(true, Ordering::SeqCst),
        );

        assert!(
            hard_ran.load(Ordering::SeqCst),
            "hard_exit MUST run on the second signal"
        );
    }

    #[test]
    fn watcher_no_signal_is_a_noop() {
        // Pin: if the iterator returns None first (channel closed
        // before any signal), the loop must not spawn graceful or
        // call hard_exit.
        use std::sync::atomic::{AtomicBool, Ordering};

        let graceful_ran = Arc::new(AtomicBool::new(false));
        let hard_ran = Arc::new(AtomicBool::new(false));

        let g = graceful_ran.clone();
        let h = hard_ran.clone();

        run_watcher_loop(
            || None,
            move || g.store(true, Ordering::SeqCst),
            move || h.store(true, Ordering::SeqCst),
        );

        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!graceful_ran.load(Ordering::SeqCst));
        assert!(!hard_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn registration_guard_clears_even_on_early_return_path() {
        // Same shape as above but pinning that the guard's Drop runs
        // even when the surrounding scope exits via `?`-propagation
        // (i.e. an error path). Without the guard pattern the call
        // site has to remember to clear; with it, Drop handles every
        // exit path.
        fn inner(r: &ChildRegistry) -> Result<(), &'static str> {
            let _g = r.register_pid_with_guard(7);
            Err("simulated playback error")
        }
        let r = ChildRegistry::new();
        let _ = inner(&r);
        assert_eq!(r.current(), None);
    }
}

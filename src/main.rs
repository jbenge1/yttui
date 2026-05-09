use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use yttui::app::{Action, App};
use yttui::cli::Cli;
use yttui::dispatcher::{PendingSearch, SearchDispatcher};
use yttui::player::{MpvPlayer, PlaybackOptions, spawn_result};
use yttui::search::{SearchResult, YtDlpBackend};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() {
    // clap exits with help/validation errors on its own; we handle
    // `--version` ourselves to print LONG_VERSION cleanly (no auto-prefix).
    let cli = Cli::parse();

    if cli.version {
        print!("{}", yttui::cli::LONG_VERSION);
        return;
    }

    if let Err(e) = yttui::preflight::check() {
        eprintln!("yttui: {e}");
        std::process::exit(2);
    }
    // Load config *before* logger init so the logger can honor
    // `[log] level`. The wrinkle: a config-load failure still needs
    // to be logged, but the logger isn't up yet — defer the warning
    // to a String and emit it after `init_logger` has a sink. Stderr
    // would corrupt the TUI once alt-screen is up; the deferred path
    // keeps that one warning routed to the file like every other
    // diagnostic.
    let (config, deferred_load_warning) =
        match yttui::config::Config::load_from_default_path() {
            Ok(c) => (c, None),
            Err(e) => {
                // Walk the source chain so the underlying io/toml detail
                // shows up in the log; `ConfigError`'s Display intentionally
                // doesn't embed `{source}` (see #84). A1.3 owns the proper
                // user-facing error UX.
                let mut msg = e.to_string();
                let mut src: Option<&dyn std::error::Error> =
                    std::error::Error::source(&e);
                while let Some(s) = src {
                    msg.push_str(": ");
                    msg.push_str(&s.to_string());
                    src = s.source();
                }
                (
                    yttui::config::Config::default(),
                    Some(format!(
                        "config load failed, using defaults: {msg}"
                    )),
                )
            }
        };
    init_logger(config.log.level.into());
    if let Some(msg) = deferred_load_warning {
        log::warn!("{msg}");
    }
    install_panic_hook();
    // Install the SIGINT/SIGTERM watcher *before* setup_terminal so a
    // Ctrl-C that races against startup still leaves a clean tty
    // behind. The watcher kills any registered child group (mpv) on
    // signal — V1 spec AC #4. See `yttui::signal` for the full why.
    if let Err(e) = yttui::signal::install_handler(
        &yttui::signal::REGISTRY,
        signal_cleanup,
    ) {
        // Non-fatal: we lose the orphan-prevention guarantee for the
        // signal path, but the rest of the app still works. Warn loud
        // since this is a real degradation of correctness.
        eprintln!(
            "yttui: warning: could not install signal handler: {e} \
             (Ctrl-C during playback may leak mpv)"
        );
    }
    // Setup/restore/run errors must surface with the same `yttui:`
    // prefix everywhere. Returning `Err(_)` from `main` would route
    // through `Termination::report`, which double-prints a debug-format
    // `Error: Os { … }`. Exit explicitly instead.
    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("yttui: {e}");
            std::process::exit(1);
        }
    };
    let run_result = run(&mut terminal, &cli, &config);
    let restore_result = restore_terminal(&mut terminal);

    let final_err = run_result.err().or_else(|| restore_result.err());
    if let Some(e) = final_err {
        eprintln!("yttui: {e}");
        std::process::exit(1);
    }
}

fn run(
    terminal: &mut Term,
    cli: &Cli,
    config: &yttui::config::Config,
) -> io::Result<()> {
    let dispatcher = SearchDispatcher::new(YtDlpBackend::default());
    let player = MpvPlayer::default();
    // Cloning the args vec once at startup; the `PlaybackOptions`
    // value is reused for every play.
    let opts = PlaybackOptions::default()
        .with_audio_only(cli.audio_only)
        .with_extra_args(config.player.args.clone());

    let mut app = App::new();
    let mut pending_search: Option<PendingSearch> = None;

    // If we got an initial query on the CLI, fire it immediately.
    if let Some(q) = cli.initial_query() {
        app.input = q;
        if let Action::StartSearch(query) = app.commit_query() {
            pending_search =
                Some(dispatcher.dispatch(query, cli.count, cli.recent));
        }
    }

    loop {
        terminal.draw(|frame| yttui::tui::draw(frame, &mut app))?;

        // Drain any completed search before polling input. The
        // dispatcher owns the Disconnected→WorkerPanicked mapping
        // (#73) so this consumer no longer has to construct any
        // DispatchError variants directly.
        if let Some(p) = &pending_search {
            match p.try_recv() {
                Ok(Some((seq, outcome))) => {
                    // The `seq != current_seq()` arm is currently
                    // unreachable in production: `pending_search` is
                    // overwritten by every new dispatch and nulled by
                    // every cancel, so any outcome we read here was
                    // produced by the dispatch that set
                    // `current_seq()`. The guard is kept defensively in
                    // case a future refactor introduces a second
                    // dispatch path or detaches `pending_search` from
                    // the seq counter — see
                    // `dispatcher::tests::stale_result_is_identifiable_via_seq_compare`
                    // which simulates that scenario explicitly.
                    if seq == dispatcher.current_seq() {
                        // Note: `SearchError::Cancelled` is unreachable
                        // here in practice — `Action::CancelSearch` drops
                        // `pending_search` before the worker can send,
                        // so the outcome is discarded by the channel.
                        match outcome {
                            Ok(results) => app.set_results(results),
                            // `#[from]` on `DispatchError::Search` lets
                            // the bare backend error lift via `into()`
                            // without naming the variant — keeps this
                            // call site free of dispatcher-internal
                            // variant construction.
                            Err(e) => app.set_search_error(Arc::new(
                                e.into(),
                            )),
                        }
                    }
                    pending_search = None;
                }
                Ok(None) => {}
                Err(e) => {
                    // Worker thread dropped its sender without sending
                    // — i.e. its closure panicked. Surface so the user
                    // doesn't see a perma-spinner. `e` is the
                    // dispatcher-constructed `DispatchError::WorkerPanicked`.
                    app.set_search_error(Arc::new(e));
                    pending_search = None;
                }
            }
        }

        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }

        let key = match event::read()? {
            Event::Key(k) if k.kind == event::KeyEventKind::Press => k,
            // Other events (resize, focus, mouse, paste, key-release) just
            // trigger another draw on the next loop iteration.
            _ => continue,
        };

        // `Action::None` and the `_` wildcard arm have identical
        // bodies, but we want them spelled separately: `None` is the
        // real "nothing happened" path; `_` exists only because
        // `Action` is `#[non_exhaustive]` and future variants must
        // not be silently swallowed without a deliberate decision here.
        #[allow(clippy::match_same_arms)]
        match app.handle_key(key) {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::StartSearch(q) => {
                pending_search =
                    Some(dispatcher.dispatch(q, cli.count, cli.recent));
            }
            Action::Rerun => {
                if let Some(q) = app.committed_query.clone() {
                    pending_search =
                        Some(dispatcher.dispatch(q, cli.count, cli.recent));
                }
            }
            Action::CancelSearch => {
                if let Some(p) = &pending_search {
                    dispatcher.cancel(p);
                }
                pending_search = None;
            }
            Action::Play(result) => {
                if let Err(e) = play_with_swap(terminal, &player, &result, &opts)
                {
                    app.set_player_error(Arc::new(e));
                }
            }
            // `Action` is `#[non_exhaustive]`. Future variants must
            // not silently fall off the loop; we treat them as a
            // no-op until handled explicitly.
            _ => {}
        }
    }
}

fn play_with_swap(
    terminal: &mut Term,
    player: &MpvPlayer,
    result: &SearchResult,
    opts: &PlaybackOptions,
) -> Result<(), yttui::player::PlayerError> {
    // #30 contract: leave alt screen so mpv can take the terminal cleanly,
    // restore on return. We log loud on either failure but don't abort
    // playback — leaving alt screen partially can produce visual junk,
    // but the re-enter side will clear() and recover.
    if let Err(e) = restore_terminal(terminal) {
        log::warn!("failed to leave alternate screen before mpv: {e}");
    }
    // `register_spawn` holds the registry mutex across the spawn so a
    // SIGINT in the spawn-vs-register window cannot observe a transient
    // `None` (#70). `_guard`'s Drop clears the registry on every exit
    // path of this function, which structurally prevents the
    // "registered, but child already reaped" leak (#71); the residual
    // window between `wait()` returning and Drop is bounded to a
    // handful of unwind instructions.
    let outcome = match yttui::signal::REGISTRY.register_spawn::<_, _, _>(|| {
        let running = spawn_result(player, result, opts)?;
        let pid = running.pid();
        Ok::<_, yttui::player::PlayerError>((running, pid))
    }) {
        Ok((running, _guard)) => running.wait(),
        Err(e) => Err(e),
    };
    if let Err(e) = re_enter_terminal(terminal) {
        log::warn!("failed to re-enter alternate screen after mpv: {e}");
    }
    outcome
}

fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn re_enter_terminal(terminal: &mut Term) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen, Hide)?;
    terminal.clear()?;
    Ok(())
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    Ok(())
}

/// Initialize a file logger so `log::warn!` calls (malformed yt-dlp
/// entries, terminal-restore failures, etc.) actually land somewhere.
/// Stderr would corrupt the TUI, so we write to a file under the
/// platform cache dir. Best-effort: if anything fails we print a
/// one-line warning to stderr (terminal isn't in alt-screen yet, so
/// stderr is safe here) and continue — `log::warn!` calls become
/// no-ops in that case, but the TUI itself still works.
///
/// `level` is sourced from `[log] level` in the user config; the
/// default matches the level previously hardcoded here, so V1
/// behavior is unchanged unless the user opts in.
fn init_logger(level: log::LevelFilter) {
    let Some(cache) = dirs::cache_dir() else {
        eprintln!(
            "yttui: warning: no cache directory available; logs disabled"
        );
        return;
    };
    let log_dir = cache.join("yttui");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "yttui: warning: cannot create log dir {}: {e}; logs disabled",
            log_dir.display()
        );
        return;
    }
    let log_path = log_dir.join("yttui.log");
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "yttui: warning: cannot open log file {}: {e}; logs disabled",
                log_path.display()
            );
            return;
        }
    };
    let _ = simplelog::WriteLogger::init(
        level,
        simplelog::Config::default(),
        file,
    );
}

/// Cleanup invoked by the signal watcher just before
/// `process::exit(130)`. Best-effort terminal restore so the user's
/// shell isn't left in raw mode with the alt screen up. Mirrors
/// [`install_panic_hook`].
fn signal_cleanup() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Best-effort terminal restoration on panic so the user's shell isn't
/// left in raw mode with the alt screen up.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        original(info);
    }));
}


use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
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
use yttui::player::{MpvPlayer, PlaybackOptions, play_result};
use yttui::search::{
    SearchBackend, SearchError, SearchResult, SortOrder, YtDlpBackend,
};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

type Term = Terminal<CrosstermBackend<Stdout>>;
type SearchOutcome = (u64, Result<Vec<SearchResult>, SearchError>);

struct PendingSearch {
    rx: Receiver<SearchOutcome>,
    cancel: Arc<AtomicBool>,
}

fn main() -> io::Result<()> {
    // clap exits with help/version/validation errors on its own, so this
    // call either returns or terminates the process for us.
    let cli = Cli::parse();

    if let Err(e) = yttui::preflight::check() {
        eprintln!("yttui: {e}");
        std::process::exit(2);
    }
    init_logger();
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, &cli);
    restore_terminal(&mut terminal)?;
    if let Err(e) = &result {
        eprintln!("yttui: {e}");
    }
    result
}

fn run(terminal: &mut Term, cli: &Cli) -> io::Result<()> {
    let backend = YtDlpBackend::default();
    let player = MpvPlayer::default();
    let opts = PlaybackOptions::default().with_audio_only(cli.audio_only);

    let mut app = App::new();
    let mut pending_search: Option<PendingSearch> = None;
    let search_seq = Arc::new(AtomicU64::new(0));

    // If we got an initial query on the CLI, fire it immediately.
    if let Some(q) = cli.initial_query() {
        app.input = q;
        if let Action::StartSearch(query) = app.commit_query() {
            pending_search = Some(spawn_search(
                &backend,
                &search_seq,
                query,
                cli.count,
                cli.recent,
            ));
        }
    }

    loop {
        terminal.draw(|frame| yttui::tui::draw(frame, &mut app))?;

        // Drain any completed search before polling input.
        if let Some(p) = &pending_search {
            match p.rx.try_recv() {
                Ok((seq, outcome)) => {
                    let current = search_seq.load(Ordering::SeqCst);
                    if seq == current {
                        // Note: `SearchError::Cancelled` is unreachable
                        // here in practice — `Action::CancelSearch` drops
                        // `pending_search` before the worker can send,
                        // so the outcome is discarded by the channel.
                        match outcome {
                            Ok(results) => app.set_results(results),
                            Err(e) => app.set_search_error(Arc::new(e)),
                        }
                    }
                    pending_search = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // The worker dropped its sender without sending —
                    // i.e. the closure panicked. Surface this so the
                    // user doesn't see a perma-spinner.
                    app.set_search_error(Arc::new(SearchError::WorkerPanicked));
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

        match app.handle_key(key) {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::StartSearch(q) => {
                pending_search = Some(spawn_search(
                    &backend,
                    &search_seq,
                    q,
                    cli.count,
                    cli.recent,
                ));
            }
            Action::Rerun => {
                if let Some(q) = app.committed_query.clone() {
                    pending_search = Some(spawn_search(
                        &backend,
                        &search_seq,
                        q,
                        cli.count,
                        cli.recent,
                    ));
                }
            }
            Action::CancelSearch => {
                // Trip the cancel flag so the worker actively kills the
                // yt-dlp process group, then bump the seq counter so
                // any result already in flight is discarded.
                if let Some(p) = &pending_search {
                    p.cancel.store(true, Ordering::SeqCst);
                }
                search_seq.fetch_add(1, Ordering::SeqCst);
                pending_search = None;
            }
            Action::Play(result) => {
                if let Err(e) = play_with_swap(terminal, &player, &result, &opts)
                {
                    app.set_player_error(Arc::new(e));
                }
            }
        }
    }
}

fn spawn_search(
    backend: &YtDlpBackend,
    seq: &Arc<AtomicU64>,
    query: String,
    count: u32,
    recent: bool,
) -> PendingSearch {
    let (tx, rx) = mpsc::channel();
    let backend = backend.clone();
    let this_seq = seq.fetch_add(1, Ordering::SeqCst) + 1;
    let sort = if recent {
        SortOrder::Date
    } else {
        SortOrder::Relevance
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    thread::spawn(move || {
        let outcome = backend.search(&query, count, sort, &cancel_clone);
        // Best-effort send; receiver may be gone if the user moved on.
        let _ = tx.send((this_seq, outcome));
    });
    PendingSearch { rx, cancel }
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
    let outcome = play_result(player, result, opts);
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
fn init_logger() {
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
        log::LevelFilter::Warn,
        simplelog::Config::default(),
        file,
    );
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


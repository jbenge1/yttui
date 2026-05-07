//! Search worker orchestration. Lifted out of `main.rs` so the
//! seq-counter race-discard pattern is unit-testable without touching
//! the terminal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use thiserror::Error;

use crate::search::{SearchBackend, SearchError, SearchResult, SortOrder};

/// Errors surfaced by the dispatcher to the main loop.
///
/// Distinct from [`SearchError`] because some failure modes (e.g. the
/// worker thread panicking before producing any outcome) are
/// dispatch-layer concerns the backend cannot itself construct.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DispatchError {
    /// The backend itself returned an error.
    #[error(transparent)]
    Search(#[from] SearchError),
    /// The worker thread dropped its sender without producing an
    /// outcome — i.e. the backend closure panicked.
    #[error("search worker thread crashed before producing a result")]
    WorkerPanicked,
}

pub type SearchOutcome = (u64, Result<Vec<SearchResult>, SearchError>);

#[derive(Debug)]
pub struct PendingSearch {
    pub rx: Receiver<SearchOutcome>,
    pub cancel: Arc<AtomicBool>,
}

/// Owns the running-search bookkeeping: a sequence counter the main
/// loop consults to discard stale results, and the backend used to do
/// the actual work.
#[derive(Debug)]
pub struct SearchDispatcher<B> {
    backend: B,
    seq: Arc<AtomicU64>,
}

impl<B: SearchBackend + Clone + Send + 'static> SearchDispatcher<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The current sequence value. Used by the main loop to compare
    /// against the seq returned by a worker.
    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    /// Cancel an in-flight search: trip the cancel flag (so the
    /// backend kills its subprocess group on the next poll) and bump
    /// the sequence so any result still in flight will be treated as
    /// stale by `current_seq()` comparisons.
    pub fn cancel(&self, pending: &PendingSearch) {
        pending.cancel.store(true, Ordering::SeqCst);
        self.seq.fetch_add(1, Ordering::SeqCst);
    }

    /// Spawn a worker for a new search. Returns a `PendingSearch` the
    /// caller polls via `try_recv` and can pass back to [`Self::cancel`].
    pub fn dispatch(
        &self,
        query: String,
        count: u32,
        recent: bool,
    ) -> PendingSearch {
        let (tx, rx) = mpsc::channel();
        let backend = self.backend.clone();
        let this_seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let sort = if recent {
            SortOrder::Date
        } else {
            SortOrder::Relevance
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        thread::spawn(move || {
            let outcome =
                backend.search(&query, count, sort, &cancel_clone);
            // Best-effort send; receiver may be gone if the user moved on.
            let _ = tx.send((this_seq, outcome));
        });
        PendingSearch { rx, cancel }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchResult, VideoDuration};
    use std::sync::{Barrier, Mutex};

    /// Backend whose `search` blocks on a barrier so tests can interleave
    /// dispatches and inspect bookkeeping mid-flight.
    #[derive(Clone)]
    struct BarrierBackend {
        barrier: Arc<Barrier>,
        cancel_observed: Arc<Mutex<bool>>,
    }

    impl BarrierBackend {
        fn new(barrier_count: usize) -> Self {
            Self {
                barrier: Arc::new(Barrier::new(barrier_count)),
                cancel_observed: Arc::new(Mutex::new(false)),
            }
        }
    }

    impl SearchBackend for BarrierBackend {
        fn search(
            &self,
            query: &str,
            _count: u32,
            _sort: SortOrder,
            cancel: &AtomicBool,
        ) -> Result<Vec<SearchResult>, SearchError> {
            self.barrier.wait();
            if cancel.load(Ordering::SeqCst) {
                *self.cancel_observed.lock().unwrap() = true;
                return Err(SearchError::Cancelled);
            }
            Ok(vec![SearchResult {
                id: format!("for-{query}"),
                title: query.to_string(),
                channel: None,
                duration: VideoDuration::Seconds(60),
            }])
        }
    }

    /// Backend that returns immediately. Used when we only care about
    /// seq bookkeeping, not interleaving.
    #[derive(Clone)]
    struct InstantBackend;

    impl SearchBackend for InstantBackend {
        fn search(
            &self,
            query: &str,
            _count: u32,
            _sort: SortOrder,
            _cancel: &AtomicBool,
        ) -> Result<Vec<SearchResult>, SearchError> {
            Ok(vec![SearchResult {
                id: query.to_string(),
                title: query.to_string(),
                channel: None,
                duration: VideoDuration::Seconds(0),
            }])
        }
    }

    #[test]
    fn dispatch_error_wraps_search_error_via_from() {
        // Sanity: any `SearchError` lifts cleanly into `DispatchError`
        // through the `#[from]` bridge so call sites don't need
        // bespoke conversion.
        let se = SearchError::Cancelled;
        let de: DispatchError = se.into();
        assert!(matches!(de, DispatchError::Search(SearchError::Cancelled)));
    }

    #[test]
    fn dispatch_error_worker_panicked_renders_a_useful_message() {
        let de = DispatchError::WorkerPanicked;
        let s = de.to_string();
        assert!(s.contains("worker"));
        assert!(s.contains("crashed") || s.contains("panic"));
    }

    #[test]
    fn dispatch_advances_seq_monotonically() {
        let d = SearchDispatcher::new(InstantBackend);
        assert_eq!(d.current_seq(), 0);
        let _p1 = d.dispatch("a".into(), 1, false);
        assert_eq!(d.current_seq(), 1);
        let _p2 = d.dispatch("b".into(), 1, false);
        assert_eq!(d.current_seq(), 2);
    }

    #[test]
    fn outcome_is_tagged_with_dispatch_seq() {
        let d = SearchDispatcher::new(InstantBackend);
        let p1 = d.dispatch("a".into(), 1, false);
        let p2 = d.dispatch("b".into(), 1, false);
        let (seq1, out1) = p1.rx.recv().unwrap();
        let (seq2, out2) = p2.rx.recv().unwrap();
        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(out1.unwrap()[0].id, "a");
        assert_eq!(out2.unwrap()[0].id, "b");
    }

    #[test]
    fn cancel_bumps_seq_and_trips_flag() {
        // Barrier of 2 keeps the worker parked so we can inspect the
        // cancel flag mid-flight.
        let backend = BarrierBackend::new(2);
        let observed = backend.cancel_observed.clone();
        let barrier = backend.barrier.clone();
        let d = SearchDispatcher::new(backend);
        let p = d.dispatch("a".into(), 1, false);
        let before = d.current_seq();
        d.cancel(&p);
        assert!(p.cancel.load(Ordering::SeqCst));
        assert_eq!(d.current_seq(), before + 1);
        // Release the worker so it observes the cancel flag.
        barrier.wait();
        let (_seq, outcome) = p.rx.recv().unwrap();
        assert!(matches!(outcome, Err(SearchError::Cancelled)));
        assert!(*observed.lock().unwrap());
    }

    #[test]
    fn stale_result_is_identifiable_via_seq_compare() {
        // Simulates: dispatch #1, dispatch #2 (overrides), result #1
        // arrives and is identified as stale.
        let d = SearchDispatcher::new(InstantBackend);
        let p1 = d.dispatch("a".into(), 1, false);
        let _p2 = d.dispatch("b".into(), 1, false);
        let (seq1, _) = p1.rx.recv().unwrap();
        // Main loop's check: seq1 == current_seq()? No → discard.
        assert_ne!(seq1, d.current_seq());
    }

    #[test]
    fn cancel_then_new_dispatch_invalidates_old_seq() {
        // Models the real flow: user types query, hits Esc, types
        // another. The first worker's outcome (had it survived) would
        // arrive tagged with a stale seq, which the main loop
        // identifies and discards. Same dispatcher across both — the
        // shared seq counter is the whole point.
        let d = SearchDispatcher::new(InstantBackend);
        let p1 = d.dispatch("a".into(), 1, false);
        d.cancel(&p1);
        let p2 = d.dispatch("b".into(), 1, false);

        let (p1_seq, _) = p1.rx.recv().unwrap();
        let (p2_seq, _) = p2.rx.recv().unwrap();

        // p1 was dispatched at seq=1. Cancel bumped to 2. p2 dispatched
        // at seq=3. So p1's outcome is stale relative to the dispatcher's
        // current view.
        assert_eq!(p1_seq, 1);
        assert_eq!(p2_seq, 3);
        assert_eq!(d.current_seq(), 3);
        assert_ne!(p1_seq, d.current_seq(), "p1 must read as stale");
        assert_eq!(p2_seq, d.current_seq(), "p2 must read as fresh");
    }
}

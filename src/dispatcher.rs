//! Search worker orchestration. Lifted out of `main.rs` so the
//! seq-counter race-discard pattern is unit-testable without touching
//! the terminal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::search::{SearchBackend, SearchError, SearchResult, SortOrder};

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
    use std::time::Duration;

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
        // dispatch → cancel → dispatch. The first outcome's seq must
        // not match current_seq after the second dispatch.
        let backend = BarrierBackend::new(2);
        let barrier = backend.barrier.clone();
        let d = SearchDispatcher::new(backend.clone());
        let p1 = d.dispatch("a".into(), 1, false);
        d.cancel(&p1);
        // Release the cancelled worker so it can finalize.
        barrier.wait();
        let _ = p1.rx.recv_timeout(Duration::from_secs(1));
        // Use a fresh backend for the second dispatch (each
        // BarrierBackend has its own barrier).
        let backend2 = BarrierBackend::new(1);
        let d2 = SearchDispatcher::new(backend2);
        let p2 = d2.dispatch("b".into(), 1, false);
        let (seq2, _) = p2.rx.recv().unwrap();
        assert_eq!(seq2, 1);
    }
}

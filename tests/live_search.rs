//! Live integration test that hits the network via real `yt-dlp`.
//! Run with `cargo test --test live_search -- --ignored`.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::AtomicBool;

use yttui::search::{SearchBackend, SortOrder, YtDlpBackend};

#[test]
#[ignore = "hits the network; run with --ignored"]
fn live_search_returns_results() {
    let backend = YtDlpBackend::default();
    let cancel = AtomicBool::new(false);
    let results = backend
        .search("rust ratatui", 3, SortOrder::Relevance, &cancel)
        .unwrap();
    assert!(!results.is_empty(), "expected at least one result");
}

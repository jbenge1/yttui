//! Live integration test that hits the network via real `yt-dlp`.
//! Run with `cargo test --test live_search -- --ignored`.

#![allow(clippy::unwrap_used)]

use yttui::search::{SortOrder, YtDlpBackend, search};

#[test]
#[ignore = "hits the network; run with --ignored"]
fn live_search_returns_results() {
    let backend = YtDlpBackend::default();
    let results =
        search(&backend, "rust ratatui", 3, SortOrder::Relevance).unwrap();
    assert!(!results.is_empty(), "expected at least one result");
}

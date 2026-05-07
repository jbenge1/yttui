//! Binary-level smoke tests. These run the actual built binary so a
//! regression in clap setup, version intercept, preflight, or process
//! exit handling can't sneak past the unit suite.
//!
//! Network-free, terminal-free — every test exits via clap or via the
//! `--version` intercept before TUI setup runs.

#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_flag_prints_ascii_art_and_exits_zero() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("yttui v"))
        .stdout(predicate::str::contains("Justin Benge"))
        .stdout(predicate::str::contains("MIT License"));
}

#[test]
fn short_v_alias_works() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("-v")
        .assert()
        .success()
        .stdout(predicate::str::contains("yttui v"));
}

#[test]
fn capital_v_alias_works() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains("yttui v"));
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("--recent"))
        .stdout(predicate::str::contains("--count"))
        .stdout(predicate::str::contains("--audio-only"));
}

#[test]
fn help_does_not_leak_internal_rationale_about_v_alias() {
    // The `-v`-as-`--version` rationale is a note for future maintainers,
    // not end users. It must not appear in --help output.
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("one-way door").not())
        .stdout(predicate::str::contains("pacman-style").not())
        .stdout(predicate::str::contains("future verbosity").not());
}

#[test]
fn count_below_minimum_is_rejected() {
    Command::cargo_bin("yttui")
        .unwrap()
        .args(["--count", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn count_above_maximum_is_rejected() {
    Command::cargo_bin("yttui")
        .unwrap()
        .args(["--count", "101"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn non_tty_setup_failure_uses_yttui_prefix_not_debug_format() {
    // Repro from second-opinion review: `yttui </dev/null` was emitting
    // `Error: Os { code: 6, … }` (Termination::report's debug print)
    // instead of the user-friendly `yttui: <message>` we use everywhere
    // else. assert_cmd already runs the child without a controlling
    // terminal, so the setup_terminal path that needs a TTY blows up
    // for any reason and we get to assert on the prefix.
    //
    // Why a query rather than `--version`: --version exits before
    // setup_terminal. We need to actually reach terminal setup. The
    // preflight may fail first if yt-dlp/mpv aren't installed on the CI
    // runner, but that's also `yttui:`-prefixed, so the assertion holds.
    let assert = Command::cargo_bin("yttui")
        .unwrap()
        .arg("rust")
        .write_stdin("")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("yttui:"),
        "expected `yttui:` prefix in stderr, got: {stderr}"
    );
    assert!(
        !stderr.contains("Error: Os {"),
        "debug-formatted Error leaked through: {stderr}"
    );
}

#[test]
fn unknown_flag_is_rejected() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("--definitely-not-a-flag")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

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
fn unknown_flag_is_rejected() {
    Command::cargo_bin("yttui")
        .unwrap()
        .arg("--definitely-not-a-flag")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

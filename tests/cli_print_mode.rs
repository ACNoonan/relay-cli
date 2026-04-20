//! Flag-validation tests for `relay chat --print` (print/non-interactive mode).
//!
//! These tests deliberately do NOT exercise a real backend. The print path
//! shells out to `claude` / `codex` / OpenAI, none of which can run in CI
//! without auth and binaries on PATH. Limit to surface that fails fast:
//! - Mutually-exclusive flags (`--print` + `--resume`).
//! - Rotation-list parsing (empty / unknown agent names).

use assert_cmd::Command;
use predicates::prelude::*;

fn relay() -> Command {
    Command::cargo_bin("relay").unwrap()
}

#[test]
fn chat_help_documents_print_mode_flags() {
    relay()
        .args(["chat", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--print"))
        .stdout(predicate::str::contains("--rotation"))
        .stdout(predicate::str::contains("--format"));
}

#[test]
fn print_with_resume_is_rejected() {
    // Mutually exclusive — clap should reject before any backend kicks off.
    relay()
        .args([
            "chat",
            "--print",
            "hello",
            "--resume",
            "00000000-0000-0000-0000-000000000000",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn rotation_without_print_is_rejected() {
    // `--rotation` only makes sense in print mode (it `requires = "print"`).
    relay()
        .args(["chat", "--rotation", "claude,codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--print"));
}

#[test]
fn format_without_print_is_rejected() {
    // Same constraint for `--format`: only meaningful alongside `--print`.
    relay()
        .args(["chat", "--format", "json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--print"));
}

#[test]
fn print_with_empty_rotation_is_rejected() {
    relay()
        .args(["chat", "--print", "hello", "--rotation", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--rotation cannot be empty"));
}

#[test]
fn print_with_unknown_agent_in_rotation_is_rejected() {
    relay()
        .args([
            "chat",
            "--print",
            "hello",
            "--rotation",
            "claude,gemini,codex",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("gemini"));
}

#[test]
fn print_with_invalid_format_is_rejected_by_clap() {
    // clap's own value_parser rejects this before our code runs.
    relay()
        .args(["chat", "--print", "hello", "--format", "yaml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("yaml"));
}

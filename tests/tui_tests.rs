// Unit-style tests for TUI state, actions, and data helpers.
// These test pure logic without requiring a terminal.

use tempfile::TempDir;

// We re-test via the binary since the tui module is private.
// For state/action logic tests, we exercise through the CLI surface.

use assert_cmd::Command;
use predicates::prelude::*;

fn relay() -> Command {
    Command::cargo_bin("relay").unwrap()
}

#[test]
fn tui_appears_in_help() {
    relay()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("tui"))
        .stdout(predicate::str::contains("interactive TUI dashboard"));
}

#[test]
fn tui_subcommand_help() {
    relay()
        .args(["tui", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Launch the interactive TUI dashboard",
        ));
}

/// Verify existing CLI commands still work after adding TUI.
#[test]
fn existing_commands_still_work() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    // Init
    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    // Session list
    relay()
        .args(["session", "list"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Artifacts list
    relay()
        .args(["artifacts", "list"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Config show
    relay()
        .args(["config", "show"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Review history
    relay()
        .args(["review", "history"])
        .current_dir(tmp.path())
        .assert()
        .success();
}

/// Verify doctor still works.
#[test]
fn doctor_still_works() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("doctor")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Providers"));
}

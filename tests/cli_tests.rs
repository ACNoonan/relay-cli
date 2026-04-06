use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn relay() -> Command {
    Command::cargo_bin("relay").unwrap()
}

#[test]
fn help_shows_all_commands() {
    relay()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("session"))
        .stdout(predicate::str::contains("capture"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("test"))
        .stdout(predicate::str::contains("commit"))
        .stdout(predicate::str::contains("ci"))
        .stdout(predicate::str::contains("artifacts"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn version_flag_works() {
    relay()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("relay"));
}

#[test]
fn init_creates_harness_dir() {
    let tmp = TempDir::new().unwrap();

    // Init a git repo first.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized relay harness"));

    // Verify directories were created.
    assert!(tmp.path().join(".agent-harness").is_dir());
    assert!(tmp.path().join(".agent-harness/config.toml").is_file());
    assert!(tmp.path().join(".agent-harness/sessions").is_dir());
    assert!(tmp.path().join(".agent-harness/artifacts").is_dir());
    assert!(tmp.path().join(".agent-harness/handoffs").is_dir());
    assert!(tmp.path().join(".agent-harness/logs").is_dir());
}

#[test]
fn init_idempotent() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    // First init.
    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    // Second init should not fail.
    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("already initialized"));
}

#[test]
fn doctor_runs_without_init() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    // Doctor should work even without init (just report missing storage).
    relay()
        .arg("doctor")
        .current_dir(tmp.path())
        .assert()
        .success();
}

#[test]
fn session_list_requires_init() {
    let tmp = TempDir::new().unwrap();
    relay()
        .args(["session", "list"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not initialized"));
}

#[test]
fn artifacts_list_empty() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    relay()
        .args(["artifacts", "list"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No artifacts found"));
}

#[test]
fn session_list_empty() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    relay()
        .args(["session", "list"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No sessions found"));
}

#[test]
fn config_show_after_init() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    relay()
        .args(["config", "show"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("[workspace]"))
        .stdout(predicate::str::contains("[storage]"))
        .stdout(predicate::str::contains("[providers.claude]"));
}

#[test]
fn capture_requires_init() {
    let tmp = TempDir::new().unwrap();
    relay()
        .args(["capture", "last-response", "--file", "/dev/null"])
        .current_dir(tmp.path())
        .assert()
        .failure();
}

#[test]
fn capture_from_file() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    // Create a test file to capture.
    let test_file = tmp.path().join("response.md");
    std::fs::write(&test_file, "# Test Response\n\nHello world").unwrap();

    relay()
        .args([
            "capture",
            "last-response",
            "--file",
            test_file.to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Captured last response"));
}

#[test]
fn review_history_empty() {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();

    relay()
        .args(["review", "history"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No reviews found"));
}

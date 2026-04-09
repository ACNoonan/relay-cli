use assert_cmd::Command;
use predicates::prelude::*;
use relay_cli::schema::{ArtifactManifest, ArtifactType, CommitProposal, TestResult, Verdict};
use tempfile::TempDir;

fn relay() -> Command {
    Command::cargo_bin("relay").unwrap()
}

fn init_git_repo(tmp: &TempDir) {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
}

fn init_harness(tmp: &TempDir) {
    relay()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success();
}

fn artifact_manifest_paths(tmp: &TempDir) -> Vec<std::path::PathBuf> {
    let artifacts_dir = tmp.path().join(".agent-harness/artifacts");
    if !artifacts_dir.is_dir() {
        return vec![];
    }

    let mut paths = vec![];
    for entry in std::fs::read_dir(artifacts_dir).unwrap() {
        let entry = entry.unwrap();
        let manifest_path = entry.path().join("manifest.json");
        if manifest_path.is_file() {
            paths.push(manifest_path);
        }
    }
    paths
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
        .stdout(predicate::str::contains("config"))
        .stdout(predicate::str::contains("tui"))
        .stdout(predicate::str::contains("bridge"));
}

#[test]
fn tui_help_works() {
    relay()
        .args(["tui", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("interactive TUI dashboard"));
}

#[test]
fn bridge_help_works() {
    relay()
        .args(["bridge", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Claude -> GPT verification bridge",
        ))
        .stdout(predicate::str::contains("--prompt"))
        .stdout(predicate::str::contains("--gpt-model"));
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
    init_git_repo(&tmp);

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
    init_git_repo(&tmp);

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
    init_git_repo(&tmp);

    // Doctor should work even without init (just report missing storage).
    relay()
        .arg("doctor")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Providers"));
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
    init_git_repo(&tmp);
    init_harness(&tmp);

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
    init_git_repo(&tmp);
    init_harness(&tmp);

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
    init_git_repo(&tmp);
    init_harness(&tmp);

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
    init_git_repo(&tmp);
    init_harness(&tmp);

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
    init_git_repo(&tmp);
    init_harness(&tmp);

    relay()
        .args(["review", "history"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No reviews found"));
}

#[test]
fn capture_writes_valid_manifest_json() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(&tmp);
    init_harness(&tmp);

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
        .success();

    let manifest_paths = artifact_manifest_paths(&tmp);
    assert_eq!(manifest_paths.len(), 1);

    let data = std::fs::read_to_string(&manifest_paths[0]).unwrap();
    let manifest: ArtifactManifest = serde_json::from_str(&data).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.artifact_type, ArtifactType::LastResponse);
    assert!(manifest.path.as_std_path().is_file());
}

#[test]
fn artifacts_show_invalid_uuid_fails() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(&tmp);
    init_harness(&tmp);

    relay()
        .args(["artifacts", "show", "not-a-uuid"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid artifact ID"));
}

#[test]
fn artifacts_show_missing_uuid_fails() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(&tmp);
    init_harness(&tmp);

    relay()
        .args(["artifacts", "show", "00000000-0000-0000-0000-000000000000"])
        .current_dir(tmp.path())
        .assert()
        .failure();
}

#[test]
fn test_run_writes_test_result_json() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(&tmp);
    init_harness(&tmp);

    relay()
        .args(["test", "run", "--command", "true"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let artifacts_dir = tmp.path().join(".agent-harness/artifacts");
    let mut found = false;

    for entry in std::fs::read_dir(artifacts_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().join("test-result.json");
        if path.is_file() {
            let data = std::fs::read_to_string(path).unwrap();
            let parsed: TestResult = serde_json::from_str(&data).unwrap();
            assert_eq!(parsed.schema_version, 1);
            assert!(matches!(parsed.verdict, Verdict::Pass));
            found = true;
            break;
        }
    }

    assert!(found, "expected a saved test-result.json artifact");
}

#[test]
fn commit_prepare_writes_proposal_json() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(&tmp);
    init_harness(&tmp);

    let file_path = tmp.path().join("notes.txt");
    std::fs::write(&file_path, "first line\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Relay Test",
            "-c",
            "user.email=relay@example.com",
            "commit",
            "--quiet",
            "-m",
            "seed commit",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    std::fs::write(&file_path, "first line\nsecond line\n").unwrap();

    relay()
        .args(["commit", "prepare"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let artifacts_dir = tmp.path().join(".agent-harness/artifacts");
    let mut found = false;

    for entry in std::fs::read_dir(artifacts_dir).unwrap() {
        let entry = entry.unwrap();
        let proposal_path = entry.path().join("commit-proposal.json");
        if proposal_path.is_file() {
            let data = std::fs::read_to_string(proposal_path).unwrap();
            let proposal: CommitProposal = serde_json::from_str(&data).unwrap();
            assert_eq!(proposal.schema_version, 1);
            assert!(
                !proposal.files_changed.is_empty() || !proposal.diff_stat.trim().is_empty(),
                "proposal should record changed files or diff stat"
            );
            found = true;
            break;
        }
    }

    assert!(found, "expected a saved commit-proposal.json artifact");
}

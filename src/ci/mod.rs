use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::process;
use crate::schema::{CiStatusSnapshot, SCHEMA_VERSION};
use crate::storage::Storage;

/// Poll CI status using `gh` CLI.
pub async fn check_ci_status(storage: &Storage) -> Result<CiStatusSnapshot> {
    // Try GitHub CLI first.
    let gh_available = which::which("gh").is_ok();

    let (status, failed_jobs, raw_output) = if gh_available {
        let result = process::run_capture(
            &[
                "gh".to_string(),
                "run".to_string(),
                "list".to_string(),
                "--limit".to_string(),
                "5".to_string(),
                "--json".to_string(),
                "status,name,conclusion,headBranch".to_string(),
            ],
            None,
        )
        .await?;

        if result.exit_code == 0 {
            parse_gh_output(&result.stdout)
        } else {
            (
                "unknown".to_string(),
                vec![],
                Some(format!("{}\n{}", result.stdout, result.stderr)),
            )
        }
    } else {
        (
            "unknown".to_string(),
            vec!["gh CLI not installed — cannot poll CI".to_string()],
            None,
        )
    };

    let snapshot = CiStatusSnapshot {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        status,
        failed_jobs,
        next_action: None,
        raw_output,
    };

    // Save snapshot.
    let dir = storage.artifacts_dir().join(snapshot.id.to_string());
    std::fs::create_dir_all(dir.as_std_path()).context("creating CI snapshot dir")?;
    let path = dir.join("ci-snapshot.json");
    let json = serde_json::to_string_pretty(&snapshot).context("serializing CI snapshot")?;
    std::fs::write(path.as_std_path(), json).context("writing CI snapshot")?;

    Ok(snapshot)
}

fn parse_gh_output(stdout: &str) -> (String, Vec<String>, Option<String>) {
    if let Ok(runs) = serde_json::from_str::<Vec<serde_json::Value>>(stdout) {
        let mut failed = Vec::new();
        let mut any_running = false;

        for run in &runs {
            let status = run["status"].as_str().unwrap_or("");
            let conclusion = run["conclusion"].as_str().unwrap_or("");
            let name = run["name"].as_str().unwrap_or("unknown");

            if status == "in_progress" || status == "queued" {
                any_running = true;
            }
            if conclusion == "failure" {
                failed.push(name.to_string());
            }
        }

        let status = if any_running {
            "running".to_string()
        } else if failed.is_empty() {
            "passing".to_string()
        } else {
            "failing".to_string()
        };

        (status, failed, Some(stdout.to_string()))
    } else {
        ("parse_error".to_string(), vec![], Some(stdout.to_string()))
    }
}

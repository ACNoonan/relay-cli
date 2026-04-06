use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::git;
use crate::schema::{CommitProposal, SCHEMA_VERSION};
use crate::storage::Storage;

/// Generate a commit proposal from current git state.
pub fn prepare_commit(storage: &Storage) -> Result<CommitProposal> {
    let diff_stat = git::diff_stat()?.unwrap_or_else(|| "No changes".to_string());
    let staged = git::staged_diff()?;
    let is_dirty = git::is_dirty()?;

    if staged.is_empty() && !is_dirty {
        anyhow::bail!("No staged or unstaged changes to commit");
    }

    // Collect changed files from git status.
    let status_output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("running git status")?;
    let files_changed: Vec<String> = String::from_utf8_lossy(&status_output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let proposal = CommitProposal {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        proposed_message: String::new(), // To be filled by the agent
        risk_notes: vec![],
        files_changed,
        diff_stat,
    };

    // Save proposal.
    let dir = storage.artifacts_dir().join(proposal.id.to_string());
    std::fs::create_dir_all(dir.as_std_path()).context("creating commit proposal dir")?;
    let path = dir.join("commit-proposal.json");
    let json = serde_json::to_string_pretty(&proposal).context("serializing commit proposal")?;
    std::fs::write(path.as_std_path(), json).context("writing commit proposal")?;

    Ok(proposal)
}

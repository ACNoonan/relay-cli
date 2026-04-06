use crate::schema::GitContext;
use anyhow::{Context, Result};
use std::process::Command;

/// Check if the current directory is inside a git repository.
pub fn is_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the current branch name.
pub fn current_branch() -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running git rev-parse")?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
    } else {
        Ok(None)
    }
}

/// Get the current commit SHA.
pub fn current_sha() -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("running git rev-parse HEAD")?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
    } else {
        Ok(None)
    }
}

/// Check if the working tree is dirty.
pub fn is_dirty() -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("running git status")?;
    Ok(!output.stdout.is_empty())
}

/// Get a short diff stat.
pub fn diff_stat() -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["diff", "--stat"])
        .output()
        .context("running git diff --stat")?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    } else {
        Ok(None)
    }
}

/// Get the full diff.
pub fn full_diff() -> Result<String> {
    let output = Command::new("git")
        .args(["diff"])
        .output()
        .context("running git diff")?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get staged diff.
pub fn staged_diff() -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--cached"])
        .output()
        .context("running git diff --cached")?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Collect full git context.
pub fn collect_context() -> Result<GitContext> {
    Ok(GitContext {
        branch: current_branch()?,
        commit_sha: current_sha()?,
        is_dirty: is_dirty()?,
        diff_stat: diff_stat()?,
    })
}

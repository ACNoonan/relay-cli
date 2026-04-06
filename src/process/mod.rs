use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::Command;

/// Result of a completed process.
pub struct ProcessResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command and capture output.
pub async fn run_capture(args: &[String], cwd: Option<&str>) -> Result<ProcessResult> {
    let (program, cmd_args) = args
        .split_first()
        .context("empty command")?;

    let mut cmd = Command::new(program);
    cmd.args(cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let output = cmd.output().await.with_context(|| {
        format!("executing: {}", args.join(" "))
    })?;

    Ok(ProcessResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Spawn an interactive process (inherits stdio).
pub async fn spawn_interactive(args: &[String], cwd: Option<&str>) -> Result<i32> {
    let (program, cmd_args) = args
        .split_first()
        .context("empty command")?;

    let mut cmd = Command::new(program);
    cmd.args(cmd_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let status = cmd.status().await.with_context(|| {
        format!("spawning interactive: {}", args.join(" "))
    })?;

    Ok(status.code().unwrap_or(-1))
}

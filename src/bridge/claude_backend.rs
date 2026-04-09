use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ClaudeTurnResult {
    pub full_text: String,
    pub session_id: Option<String>,
}

#[async_trait]
pub trait ClaudeBackend: Send + Sync {
    async fn run_prompt_stream(
        &self,
        prompt: &str,
        model: Option<&str>,
        resume_session_id: Option<&str>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ClaudeTurnResult>;
}

pub struct SubprocessBackend {
    binary: String,
}

impl SubprocessBackend {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

#[async_trait]
impl ClaudeBackend for SubprocessBackend {
    async fn run_prompt_stream(
        &self,
        prompt: &str,
        model: Option<&str>,
        resume_session_id: Option<&str>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ClaudeTurnResult> {
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        if let Some(id) = resume_session_id {
            args.push("--resume".to_string());
            args.push(id.to_string());
        }

        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning `{}`", self.binary))?;

        let stdout = child
            .stdout
            .take()
            .context("failed to capture Claude stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture Claude stderr")?;

        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = String::new();
            let mut acc = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => acc.push_str(&buf),
                    Err(_) => break,
                }
            }
            acc
        });

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut full_text = String::new();
        let mut session_id: Option<String> = None;

        loop {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .await
                .context("reading Claude output")?;
            if read == 0 {
                break;
            }

            let parsed: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if session_id.is_none() {
                session_id = parsed
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        parsed
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    });
            }

            if let Some(delta) = extract_text_delta(&parsed) {
                full_text.push_str(&delta);
                on_delta(delta);
            }
        }

        let status = child.wait().await.context("waiting on Claude process")?;
        let stderr_text = stderr_task.await.unwrap_or_default();
        if !status.success() {
            bail!(
                "Claude subprocess failed (code {:?}): {}",
                status.code(),
                stderr_text.trim()
            );
        }

        Ok(ClaudeTurnResult {
            full_text,
            session_id,
        })
    }
}

fn extract_text_delta(value: &Value) -> Option<String> {
    // Common stream-json format: {"type":"content_block_delta","delta":{"text":"..."}}
    if let Some(s) = value
        .get("delta")
        .and_then(|d| d.get("text"))
        .and_then(Value::as_str)
    {
        return Some(s.to_string());
    }

    // Alternative flattened shape.
    if let Some(s) = value.get("text_delta").and_then(Value::as_str) {
        return Some(s.to_string());
    }

    // Some events include content blocks.
    value
        .get("content_block")
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

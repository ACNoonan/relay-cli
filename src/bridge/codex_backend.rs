//! Codex backend: subprocess wrapper around the `codex exec --json` CLI.
//!
//! Phase 0 measurements (codex-cli 0.118.0):
//! - First turn uses `codex exec --json --skip-git-repo-check "<prompt>"`.
//! - Resume uses `codex exec resume --json --skip-git-repo-check <thread_id> "<prompt>"`.
//! - JSONL events observed:
//!     {"type":"thread.started","thread_id":"<uuid>"}
//!     {"type":"turn.started"}
//!     {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"..."}}
//!     {"type":"turn.completed","usage":{"input_tokens":...,"cached_input_tokens":...,"output_tokens":...}}
//!   On failure: {"type":"error","message":"..."} followed by {"type":"turn.failed","error":{"message":"..."}}.
//! - Text arrives atomically in `item.completed`, not as incremental deltas — so we emit a
//!   single `BackendEvent::TextDelta` carrying the full text before `Finished`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::agent::{AgentBackend, BackendEvent, BackendInput, BackendRunResult};
use super::chat_log::ChatLog;
use super::conversation::Agent;

pub struct CodexBackend {
    binary: String,
    log: ChatLog,
}

impl CodexBackend {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            log: ChatLog::disabled(),
        }
    }

    pub fn with_log(mut self, log: ChatLog) -> Self {
        self.log = log;
        self
    }
}

#[async_trait]
impl AgentBackend for CodexBackend {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    async fn send(
        &self,
        input: BackendInput,
        events: mpsc::Sender<BackendEvent>,
    ) -> Result<BackendRunResult> {
        events
            .send(BackendEvent::Started {
                agent: Agent::Codex,
            })
            .await
            .ok();

        let resume_id = input
            .conversation
            .sessions
            .session_id_for(Agent::Codex)
            .map(str::to_owned);

        // Build argv. `exec resume <id> <prompt>` must keep the positional order.
        let mut args: Vec<String> = Vec::new();
        args.push("exec".to_string());
        if let Some(id) = &resume_id {
            args.push("resume".to_string());
            args.push("--json".to_string());
            args.push("--skip-git-repo-check".to_string());
            if let Some(m) = &input.model_override {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            args.push(id.clone());
            args.push(input.prompt.clone());
        } else {
            args.push("--json".to_string());
            args.push("--skip-git-repo-check".to_string());
            if let Some(m) = &input.model_override {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            args.push(input.prompt.clone());
        }

        let mut child = Command::new(&self.binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning `{}`", self.binary))?;

        let stdout = child
            .stdout
            .take()
            .context("failed to capture Codex stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture Codex stderr")?;

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
        let mut thread_id: Option<String> = None;
        let mut turn_error: Option<String> = None;

        loop {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .await
                .context("reading Codex output")?;
            if read == 0 {
                break;
            }

            let parsed: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match parsed.get("type").and_then(Value::as_str) {
                Some("thread.started") => {
                    if let Some(id) = parsed.get("thread_id").and_then(Value::as_str) {
                        thread_id = Some(id.to_owned());
                        events
                            .send(BackendEvent::SessionUpdated {
                                agent: Agent::Codex,
                                id: id.to_owned(),
                            })
                            .await
                            .ok();
                    }
                }
                Some("turn.started") => {}
                Some("item.completed") => {
                    let item = match parsed.get("item") {
                        Some(v) => v,
                        None => continue,
                    };
                    let is_agent_msg =
                        item.get("type").and_then(Value::as_str) == Some("agent_message");
                    if !is_agent_msg {
                        continue;
                    }
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        full_text.push_str(text);
                        events
                            .send(BackendEvent::TextDelta {
                                agent: Agent::Codex,
                                text: text.to_owned(),
                            })
                            .await
                            .ok();
                    }
                }
                Some("turn.completed") => {}
                Some("error") => {
                    // Transient reconnect noise is also emitted here — capture but don't fail
                    // unless a `turn.failed` follows.
                    if let Some(msg) = parsed.get("message").and_then(Value::as_str) {
                        events
                            .send(BackendEvent::Status {
                                agent: Agent::Codex,
                                message: format!("codex: {}", truncate(msg, 200)),
                            })
                            .await
                            .ok();
                    }
                }
                Some("turn.failed") => {
                    let msg = parsed
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Codex turn failed")
                        .to_owned();
                    turn_error = Some(msg);
                }
                _ => {}
            }
        }

        let status = child.wait().await.context("waiting on Codex process")?;
        let stderr_text = stderr_task.await.unwrap_or_default();
        self.log.write_subprocess_stderr("codex", &stderr_text);

        if let Some(msg) = turn_error {
            events
                .send(BackendEvent::Error {
                    agent: Agent::Codex,
                    message: msg.clone(),
                })
                .await
                .ok();
            bail!("Codex error: {msg}");
        }

        if !status.success() {
            let msg = format!(
                "Codex subprocess failed (code {:?}): {}",
                status.code(),
                stderr_text.trim()
            );
            events
                .send(BackendEvent::Error {
                    agent: Agent::Codex,
                    message: msg.clone(),
                })
                .await
                .ok();
            bail!(msg);
        }

        events
            .send(BackendEvent::Finished {
                agent: Agent::Codex,
                final_text: full_text.clone(),
            })
            .await
            .ok();

        Ok(BackendRunResult {
            agent: Agent::Codex,
            session_id: thread_id,
            final_text: full_text,
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut out = s[..max].to_owned();
        out.push('…');
        out
    }
}

//! Claude backend: subprocess wrapper around the `claude` CLI.
//!
//! Flags used: `-p --bare --output-format stream-json --verbose --include-partial-messages`
//! plus `--resume <id>` when a prior session id is available. `--bare` drops the
//! ~50K-token auto-discovery overhead to ~5K per turn (Phase 0 measured 6x cost drop on
//! cache-warm subsequent calls).

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

pub struct ClaudeBackend {
    binary: String,
    log: ChatLog,
}

impl ClaudeBackend {
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
impl AgentBackend for ClaudeBackend {
    fn agent(&self) -> Agent {
        Agent::Claude
    }

    async fn send(
        &self,
        input: BackendInput,
        events: mpsc::Sender<BackendEvent>,
    ) -> Result<BackendRunResult> {
        events
            .send(BackendEvent::Started {
                agent: Agent::Claude,
            })
            .await
            .ok();

        let resume_id = input
            .conversation
            .sessions
            .session_id_for(Agent::Claude)
            .map(str::to_owned);

        let mut args = vec![
            "-p".to_string(),
            input.prompt.clone(),
            "--bare".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
        ];

        if let Some(m) = &input.model_override {
            args.push("--model".to_string());
            args.push(m.clone());
        }
        if let Some(id) = &resume_id {
            args.push("--resume".to_string());
            args.push(id.clone());
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
        let mut got_stream_delta = false;

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
                if let Some(id) = extract_session_id(&parsed) {
                    session_id = Some(id.clone());
                    events
                        .send(BackendEvent::SessionUpdated {
                            agent: Agent::Claude,
                            id,
                        })
                        .await
                        .ok();
                }
            }

            let ty = parsed.get("type").and_then(Value::as_str);

            // Prefer incremental `stream_event.content_block_delta.text_delta` events.
            if ty == Some("stream_event") {
                if let Some(delta) = extract_stream_event_delta(&parsed) {
                    got_stream_delta = true;
                    full_text.push_str(&delta);
                    events
                        .send(BackendEvent::TextDelta {
                            agent: Agent::Claude,
                            text: delta,
                        })
                        .await
                        .ok();
                }
            }

            // Only consume the `assistant` aggregate when we did *not* see streaming
            // deltas — otherwise we'd emit the same text twice.
            if ty == Some("assistant") && !got_stream_delta {
                if let Some(text) = extract_assistant_text(&parsed) {
                    full_text.push_str(&text);
                    events
                        .send(BackendEvent::TextDelta {
                            agent: Agent::Claude,
                            text,
                        })
                        .await
                        .ok();
                }
            }

            if ty == Some("result") {
                let subtype = parsed.get("subtype").and_then(Value::as_str);
                if subtype == Some("error")
                    || parsed.get("is_error").and_then(Value::as_bool) == Some(true)
                {
                    let msg = parsed
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("Claude returned an error")
                        .to_owned();
                    events
                        .send(BackendEvent::Error {
                            agent: Agent::Claude,
                            message: msg.clone(),
                        })
                        .await
                        .ok();
                    bail!("Claude error: {msg}");
                }
                // `result` also carries the authoritative final text — fall back to it if
                // no deltas were emitted (e.g. partial_messages disabled).
                if full_text.is_empty() {
                    if let Some(text) = parsed.get("result").and_then(Value::as_str) {
                        full_text.push_str(text);
                        events
                            .send(BackendEvent::TextDelta {
                                agent: Agent::Claude,
                                text: text.to_owned(),
                            })
                            .await
                            .ok();
                    }
                }
            }
        }

        let status = child.wait().await.context("waiting on Claude process")?;
        let stderr_text = stderr_task.await.unwrap_or_default();
        self.log.write_subprocess_stderr("claude", &stderr_text);
        if !status.success() {
            let hint = if stderr_text.to_lowercase().contains("api key") || stderr_text.contains("401") {
                "  (hint: `--bare` requires ANTHROPIC_API_KEY; OAuth / keychain are not read)"
            } else {
                ""
            };
            let msg = format!(
                "Claude subprocess failed (code {:?}): {}{hint}",
                status.code(),
                stderr_text.trim()
            );
            events
                .send(BackendEvent::Error {
                    agent: Agent::Claude,
                    message: msg.clone(),
                })
                .await
                .ok();
            bail!(msg);
        }
        if full_text.is_empty() {
            // Subprocess exited cleanly but produced no text — surface stderr so the
            // user sees why, instead of sitting forever on "(waiting for first token…)".
            let msg = if stderr_text.trim().is_empty() {
                "Claude exited without emitting any output.".to_string()
            } else {
                format!("Claude exited without output. stderr: {}", stderr_text.trim())
            };
            events
                .send(BackendEvent::Error {
                    agent: Agent::Claude,
                    message: msg.clone(),
                })
                .await
                .ok();
            bail!(msg);
        }

        events
            .send(BackendEvent::Finished {
                agent: Agent::Claude,
                final_text: full_text.clone(),
            })
            .await
            .ok();

        Ok(BackendRunResult {
            agent: Agent::Claude,
            session_id,
            final_text: full_text,
        })
    }
}

fn extract_session_id(value: &Value) -> Option<String> {
    value
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

/// Phase 0 confirmed shape:
/// `{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}}`
fn extract_stream_event_delta(value: &Value) -> Option<String> {
    let event = value.get("event")?;
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    delta.get("text").and_then(Value::as_str).map(str::to_owned)
}

/// Phase 0 confirmed shape:
/// `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}` at block close.
fn extract_assistant_text(value: &Value) -> Option<String> {
    let content = value.get("message")?.get("content")?.as_array()?;
    let mut out = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_content_block_delta() {
        let v = json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": { "type": "text_delta", "text": "hi" }
            }
        });
        assert_eq!(extract_stream_event_delta(&v).as_deref(), Some("hi"));
    }

    #[test]
    fn parses_assistant_aggregate() {
        let v = json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "foo" },
                    { "type": "text", "text": "bar" }
                ]
            }
        });
        assert_eq!(extract_assistant_text(&v).as_deref(), Some("foobar"));
    }

    #[test]
    fn rejects_non_text_delta_events() {
        let v = json!({
            "type": "stream_event",
            "event": { "type": "message_start", "message": {} }
        });
        assert!(extract_stream_event_delta(&v).is_none());
    }

    #[test]
    fn extracts_session_id_from_init() {
        let v = json!({"type":"system","subtype":"init","session_id":"abc-123"});
        assert_eq!(extract_session_id(&v).as_deref(), Some("abc-123"));
    }
}

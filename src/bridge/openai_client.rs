//! OpenAI (GPT) backend.
//!
//! GPT has no server-side session equivalent to Claude's `--resume` or Codex's
//! `exec resume`, so we replay the local conversation history on every turn.
//! This is isolated behind the `AgentBackend` trait so the rest of the system
//! stays agnostic about whether continuity is native or replayed.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::mpsc;

use super::agent::{AgentBackend, BackendEvent, BackendInput, BackendRunResult};
use super::conversation::{Agent, Role, TurnStatus};

struct OpenAiClient {
    client: Client,
    api_key: String,
}

impl OpenAiClient {
    fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set; required for bridge GPT review")?;
        Ok(Self {
            client: Client::new(),
            api_key,
        })
    }
}

pub struct OpenAiBackend {
    client: Mutex<Option<OpenAiClient>>,
    default_model: String,
    system_prompt: String,
}

impl OpenAiBackend {
    /// Construct without contacting the environment. The `OPENAI_API_KEY` check is deferred
    /// to the first `send` call so users without a key can still use Claude and Codex.
    pub fn new(default_model: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self {
            client: Mutex::new(None),
            default_model: default_model.into(),
            system_prompt: system_prompt.into(),
        }
    }

    fn ensure_client(&self) -> Result<(Client, String)> {
        let mut guard = self.client.lock().expect("openai client mutex poisoned");
        if guard.is_none() {
            *guard = Some(OpenAiClient::from_env()?);
        }
        let c = guard.as_ref().expect("just set");
        Ok((c.client.clone(), c.api_key.clone()))
    }
}

#[async_trait]
impl AgentBackend for OpenAiBackend {
    fn agent(&self) -> Agent {
        Agent::Gpt
    }

    async fn send(
        &self,
        input: BackendInput,
        events: mpsc::Sender<BackendEvent>,
    ) -> Result<BackendRunResult> {
        events
            .send(BackendEvent::Started { agent: Agent::Gpt })
            .await
            .ok();

        let model = input
            .model_override
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        let mut messages: Vec<Value> = Vec::with_capacity(input.conversation.turns.len() + 2);
        messages.push(json!({"role": "system", "content": &self.system_prompt}));
        for turn in &input.conversation.turns {
            if turn.status == TurnStatus::Error {
                continue;
            }
            let role = match turn.role {
                Role::User | Role::Handoff => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            let prefix = match (turn.role, turn.agent) {
                (Role::Assistant, Agent::Claude) => "[From Claude] ",
                (Role::Assistant, Agent::Codex) => "[From Codex] ",
                (Role::Handoff, _) => "[Handoff] ",
                _ => "",
            };
            messages.push(json!({
                "role": role,
                "content": format!("{prefix}{}", turn.content),
            }));
        }
        messages.push(json!({"role": "user", "content": input.prompt}));

        let body = json!({
            "model": model,
            "stream": true,
            "messages": messages,
        });

        let (http, api_key) = match self.ensure_client() {
            Ok(c) => c,
            Err(err) => {
                let msg = err.to_string();
                events
                    .send(BackendEvent::Error {
                        agent: Agent::Gpt,
                        message: msg.clone(),
                    })
                    .await
                    .ok();
                bail!(msg);
            }
        };

        let response = http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
            .context("calling OpenAI Chat Completions API")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let msg = format!("OpenAI API error ({status}): {text}");
            events
                .send(BackendEvent::Error {
                    agent: Agent::Gpt,
                    message: msg.clone(),
                })
                .await
                .ok();
            bail!(msg);
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut output = String::new();

        'outer: while let Some(next) = stream.next().await {
            let bytes = next.context("reading OpenAI stream chunk")?;
            let chunk = String::from_utf8_lossy(&bytes);
            buffer.push_str(&chunk);

            while let Some(newline_idx) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_idx).collect();
                let line = line.trim();
                if !line.starts_with("data: ") {
                    continue;
                }

                let payload = line.trim_start_matches("data: ").trim();
                if payload == "[DONE]" {
                    break 'outer;
                }

                let value: Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(text) = value
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(Value::as_str)
                {
                    output.push_str(text);
                    events
                        .send(BackendEvent::TextDelta {
                            agent: Agent::Gpt,
                            text: text.to_owned(),
                        })
                        .await
                        .ok();
                }
            }
        }

        events
            .send(BackendEvent::Finished {
                agent: Agent::Gpt,
                final_text: output.clone(),
            })
            .await
            .ok();

        Ok(BackendRunResult {
            agent: Agent::Gpt,
            session_id: None,
            final_text: output,
        })
    }
}

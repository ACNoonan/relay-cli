//! Event-driven backend contract shared by Claude, Codex, and GPT.
//!
//! Each backend consumes a `BackendInput` and emits a stream of `BackendEvent`s on a channel.
//! The caller (worker state machine) uses those events to update the conversation model and
//! drive the TUI. The final `BackendRunResult` carries only metadata — actual text content
//! lives in the emitted events and on the Conversation turn.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::conversation::{Agent, Conversation};

#[derive(Debug, Clone)]
pub struct BackendInput {
    /// Prompt for this turn. For handoffs this is the wrapped handoff template.
    pub prompt: String,
    /// The full conversation so far. Backends that need to replay history (GPT) read this;
    /// backends with native resume (Claude, Codex) ignore it and pass their session id instead.
    pub conversation: Conversation,
    /// Optional per-model override provided at invocation time.
    pub model_override: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// Backend acknowledged the request and is about to contact the provider.
    Started { agent: Agent },
    /// Incremental text. Backends that receive only whole messages (Codex) fire a single
    /// `TextDelta` holding the complete content before `Finished`.
    TextDelta { agent: Agent, text: String },
    /// Backend learned its session id. For Claude this is the `session_id` from the `system`
    /// init event; for Codex the `thread_id` from `thread.started`.
    SessionUpdated { agent: Agent, id: String },
    /// Turn ended successfully. `final_text` is the full content for persistence / handoff.
    Finished { agent: Agent, final_text: String },
    /// Non-fatal status line update shown in the status bar.
    Status { agent: Agent, message: String },
    /// Turn failed. The caller should mark the current streaming turn as `TurnStatus::Error`.
    Error { agent: Agent, message: String },
}

#[derive(Debug, Clone)]
pub struct BackendRunResult {
    pub agent: Agent,
    pub session_id: Option<String>,
    pub final_text: String,
}

#[async_trait]
pub trait AgentBackend: Send + Sync {
    fn agent(&self) -> Agent;

    /// Run one turn. Must emit events on `events` *as they happen* — the caller drives
    /// the UI from the event stream concurrently, so buffering deltas until completion
    /// will stall the UI (and can deadlock if the channel fills).
    async fn send(
        &self,
        input: BackendInput,
        events: mpsc::Sender<BackendEvent>,
    ) -> Result<BackendRunResult>;
}

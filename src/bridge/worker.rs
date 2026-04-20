//! Worker state machine for the chat TUI.
//!
//! Responsibilities:
//! - Own the single `Conversation` and mutate it in response to UI commands and backend events.
//! - Dispatch to exactly one `AgentBackend` at a time.
//! - Enforce the interaction rules:
//!     * Only one backend runs at a time.
//!     * If the user types while a handoff is queued, user input wins.
//!     * Rotating agents while streaming queues the rotation; it does not interrupt.
//!     * `Ctrl+N` clears the conversation and all provider session state.
//! - Persist every turn delta to the UI via a broadcast-style event channel.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::agent::{AgentBackend, BackendEvent, BackendInput};
use super::chat_log::ChatLog;
use super::claude_backend::ClaudeBackend;
use super::codex_backend::CodexBackend;
use super::compaction::{self, CompactionConfig, CompactionResult, OpenAiSummarizer, Summarizer};
use super::conversation::{Agent, Conversation, Role, TurnStatus};
use super::openai_client::OpenAiBackend;
use super::persist::ConversationStore;

/// Runtime state of the worker. Surfaces in the TUI's status bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorkerStatus {
    #[default]
    Idle,
    Submitting {
        agent: Agent,
    },
    Streaming {
        agent: Agent,
    },
    QueuedHandoff {
        to: Agent,
    },
    Error {
        message: String,
    },
}

/// Commands the UI sends to the worker.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    /// Send user-typed input to the currently active agent.
    SendToActive { prompt: String },
    /// Rotate to the given agent. If `handoff_last_assistant` is true and a completed
    /// assistant turn exists, auto-fire a handoff prompt to the new active agent.
    RotateTo {
        agent: Agent,
        handoff_last_assistant: bool,
    },
    /// Wipe the conversation and all per-agent session state.
    NewConversation,
    /// Toggle the auto-handoff-on-rotate behaviour.
    ToggleAutoHandoff,
    /// Request worker shutdown.
    Quit,
}

/// Events the worker emits to the UI.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// Full conversation snapshot — emitted after every mutation so the UI can diff.
    ConversationUpdated(Conversation),
    /// Short status transition for the status bar.
    StatusChanged(WorkerStatus),
    /// Transient status text (non-state-changing, e.g. per-backend status lines).
    StatusMessage(String),
    /// Terminal error message — the UI shows it and the worker returns to Idle.
    Error(String),
}

pub struct WorkerConfig {
    pub claude_binary: String,
    pub codex_binary: String,
    pub claude_model: Option<String>,
    pub gpt_model: String,
    pub gpt_system_prompt: String,
    pub initial_agent: Agent,
    pub auto_handoff_enabled: bool,
    pub handoff_template: String,
    /// When Some, conversations are persisted under `.agent-harness/conversations/`.
    pub harness_root: Option<camino::Utf8PathBuf>,
    /// When Some, rehydrate this conversation instead of starting fresh.
    pub resume_conversation: Option<Conversation>,
    /// Tunables for the GPT replay-buffer compaction pass. See
    /// [`super::compaction::CompactionConfig`].
    pub compaction: CompactionConfig,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            claude_binary: "claude".into(),
            codex_binary: "codex".into(),
            claude_model: None,
            gpt_model: "gpt-5.4".into(),
            gpt_system_prompt: DEFAULT_GPT_SYSTEM_PROMPT.into(),
            initial_agent: Agent::Claude,
            auto_handoff_enabled: true,
            handoff_template: DEFAULT_HANDOFF_TEMPLATE.into(),
            harness_root: None,
            resume_conversation: None,
            compaction: CompactionConfig::default(),
        }
    }
}

pub const DEFAULT_GPT_SYSTEM_PROMPT: &str = "You are GPT-5.4 acting as a senior peer reviewer and \
collaborator in a multi-agent conversation with Claude Code and Codex. Be direct, critical, and \
specific. When reviewing another agent's output, use CRITICAL / CONCERN / SUGGESTION severity. \
When continuing work, produce concrete next steps or code.";

pub const DEFAULT_HANDOFF_TEMPLATE: &str = "Previous agent ({prev}) said:\n\n\
---\n{content}\n---\n\n\
Continue the conversation: respond, review, or extend as appropriate.";

pub fn apply_handoff_template(template: &str, prev_agent: Agent, content: &str) -> String {
    template
        .replace("{prev}", prev_agent.label())
        .replace("{content}", content)
}

pub struct Worker {
    cfg: WorkerConfig,
    conversation: Conversation,
    claude: Arc<ClaudeBackend>,
    codex: Arc<CodexBackend>,
    gpt: Arc<OpenAiBackend>,
    status: WorkerStatus,
    queued_rotation: Option<Agent>,
    events: mpsc::Sender<WorkerEvent>,
    store: ConversationStore,
    log: ChatLog,
}

impl Worker {
    pub fn new(cfg: WorkerConfig, events: mpsc::Sender<WorkerEvent>) -> Result<Self> {
        let conversation = cfg
            .resume_conversation
            .clone()
            .unwrap_or_else(|| Conversation::new(cfg.initial_agent, cfg.auto_handoff_enabled));
        let log = ChatLog::open(cfg.harness_root.as_deref(), conversation.id);
        let claude = Arc::new(ClaudeBackend::new(cfg.claude_binary.clone()).with_log(log.clone()));
        let codex = Arc::new(CodexBackend::new(cfg.codex_binary.clone()).with_log(log.clone()));
        let gpt = Arc::new(OpenAiBackend::new(
            cfg.gpt_model.clone(),
            cfg.gpt_system_prompt.clone(),
        ));
        let store = ConversationStore::open(cfg.harness_root.clone());
        Ok(Self {
            cfg,
            conversation,
            claude,
            codex,
            gpt,
            status: WorkerStatus::Idle,
            queued_rotation: None,
            events,
            store,
            log,
        })
    }

    pub fn log_path(&self) -> Option<camino::Utf8PathBuf> {
        self.log.path()
    }

    /// Manually compact the GPT replay buffer.
    ///
    /// **This is the public entry point Wave B's `/compact` slash command will call.**
    ///
    /// Module path: `relay_cli::bridge::worker::Worker::compact_gpt_history`
    /// (or `crate::bridge::worker::Worker::compact_gpt_history` from inside relay).
    ///
    /// Behaviour:
    /// - Always honours [`CompactionConfig::trigger_tokens`]: if the estimated
    ///   replay-buffer size is at or below the threshold, returns `Ok(None)`
    ///   without making an LLM call. Slash-command callers can detect this and
    ///   surface "nothing to compact" to the user.
    /// - On a triggered compaction, fires one OpenAI Chat Completions request
    ///   to summarize the older prefix, splices the result into
    ///   `self.conversation` as a single summary turn, persists, and emits
    ///   `WorkerEvent::ConversationUpdated`.
    /// - **Never** touches Claude or Codex history — those use vendor-side
    ///   `--resume` / `exec resume` ids.
    /// - Errors from the LLM call propagate; the conversation is left
    ///   untouched on failure.
    pub async fn compact_gpt_history(&mut self) -> Result<Option<CompactionResult>> {
        let summarizer = OpenAiSummarizer::from_env(self.cfg.gpt_model.clone())?;
        self.compact_gpt_history_with(&summarizer).await
    }

    /// Same as [`compact_gpt_history`] but with a caller-supplied
    /// [`Summarizer`]. Exposed for tests and for callers that want to swap in
    /// a different model/transport.
    pub async fn compact_gpt_history_with(
        &mut self,
        summarizer: &dyn Summarizer,
    ) -> Result<Option<CompactionResult>> {
        let result = compaction::compact_conversation(
            &mut self.conversation,
            &self.cfg.gpt_system_prompt,
            &self.cfg.compaction,
            summarizer,
        )
        .await?;
        if let Some(r) = &result {
            self.log.write(
                "compaction",
                &serde_json::json!({
                    "trigger": "manual",
                    "turns_summarized": r.turns_summarized,
                    "turns_remaining": r.turns_remaining,
                    "estimated_tokens_before": r.estimated_tokens_before,
                    "estimated_tokens_after": r.estimated_tokens_after,
                }),
            );
            self.emit_conversation().await;
            self.emit_status_message(format!(
                "Compacted {} GPT turns ({} → {} estimated tokens).",
                r.turns_summarized, r.estimated_tokens_before, r.estimated_tokens_after
            ))
            .await;
        }
        Ok(result)
    }

    /// Auto-trigger compaction if the replay buffer is over the configured
    /// threshold. Errors are logged and swallowed: a failed compaction must
    /// never break the user's next turn.
    async fn maybe_auto_compact(&mut self) {
        if !self.cfg.compaction.auto_enabled {
            return;
        }
        let estimated = compaction::estimate_replay_tokens(
            &self.cfg.gpt_system_prompt,
            &self.conversation.turns,
        );
        if estimated <= self.cfg.compaction.trigger_tokens {
            return;
        }
        // Only auto-compact when we have an OpenAI key; otherwise this would
        // surface a noisy error every turn for users on Claude/Codex only.
        let summarizer = match OpenAiSummarizer::from_env(self.cfg.gpt_model.clone()) {
            Ok(s) => s,
            Err(_) => return,
        };
        match compaction::compact_conversation(
            &mut self.conversation,
            &self.cfg.gpt_system_prompt,
            &self.cfg.compaction,
            &summarizer,
        )
        .await
        {
            Ok(Some(r)) => {
                self.log.write(
                    "compaction",
                    &serde_json::json!({
                        "trigger": "auto",
                        "turns_summarized": r.turns_summarized,
                        "turns_remaining": r.turns_remaining,
                        "estimated_tokens_before": r.estimated_tokens_before,
                        "estimated_tokens_after": r.estimated_tokens_after,
                    }),
                );
                self.emit_conversation().await;
                self.emit_status_message(format!(
                    "Auto-compacted {} GPT turns ({} → {} estimated tokens).",
                    r.turns_summarized, r.estimated_tokens_before, r.estimated_tokens_after
                ))
                .await;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(%err, "auto-compaction failed");
                self.log.write("compaction_error", &format!("auto: {err}"));
            }
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<WorkerCommand>) -> Result<()> {
        self.emit_conversation().await;
        self.set_status(WorkerStatus::Idle).await;

        while let Some(cmd) = commands.recv().await {
            self.log.write("worker_command", &format!("{cmd:?}"));
            match cmd {
                WorkerCommand::Quit => break,
                WorkerCommand::NewConversation => self.handle_new_conversation().await,
                WorkerCommand::ToggleAutoHandoff => self.handle_toggle_auto_handoff().await,
                WorkerCommand::RotateTo {
                    agent,
                    handoff_last_assistant,
                } => {
                    if !matches!(self.status, WorkerStatus::Idle) {
                        // Streaming/submitting: queue the rotation, apply once turn finishes.
                        self.queued_rotation = Some(agent);
                        self.emit_status_message(format!(
                            "Rotation to {} queued until current turn finishes.",
                            agent.label()
                        ))
                        .await;
                        continue;
                    }
                    self.apply_rotation(agent, handoff_last_assistant).await;
                }
                WorkerCommand::SendToActive { prompt } => {
                    if !matches!(self.status, WorkerStatus::Idle) {
                        self.emit_status_message(
                            "Busy: wait for current turn to finish before sending.".into(),
                        )
                        .await;
                        continue;
                    }
                    self.run_turn(self.conversation.active_agent, prompt, Role::User)
                        .await;
                }
            }

            // If a rotation was queued while a turn was running, apply it now that we're idle.
            if matches!(self.status, WorkerStatus::Idle) {
                if let Some(to) = self.queued_rotation.take() {
                    let do_handoff = self.conversation.auto_handoff_enabled;
                    self.apply_rotation(to, do_handoff).await;
                }
            }
        }
        Ok(())
    }

    async fn apply_rotation(&mut self, to: Agent, handoff_last_assistant: bool) {
        if to == self.conversation.active_agent {
            // Self-rotation is a no-op; avoids the "GPT → GPT" handoff-to-self bug.
            return;
        }
        self.conversation.active_agent = to;
        self.emit_conversation().await;

        if !handoff_last_assistant {
            return;
        }

        let Some(last) = self.conversation.last_assistant_turn().cloned() else {
            self.emit_status_message("No assistant turn to hand off.".into())
                .await;
            return;
        };

        // Attribute the handoff to whoever AUTHORED the content, not whoever happened
        // to be active when the user hit rotate. When the previously-active agent's
        // turns errored, `last_assistant_turn` falls back to an earlier agent — and
        // the template must reflect that, otherwise we end up saying e.g.
        // "Previous agent (GPT) said: [Claude's words]".
        let prev_agent = last.agent;
        let prompt = apply_handoff_template(&self.cfg.handoff_template, prev_agent, &last.content);
        // Record the handoff as a synthetic turn so future replays include it.
        let handoff_turn =
            super::conversation::Turn::new(to, Role::Handoff, prompt.clone(), TurnStatus::Complete);
        self.conversation.append_turn(handoff_turn);
        self.emit_conversation().await;

        self.run_turn(to, prompt, Role::Handoff).await;
    }

    async fn handle_new_conversation(&mut self) {
        self.conversation.clear();
        self.queued_rotation = None;
        self.emit_conversation().await;
        self.emit_status_message("Started a new conversation.".into())
            .await;
    }

    async fn handle_toggle_auto_handoff(&mut self) {
        self.conversation.auto_handoff_enabled = !self.conversation.auto_handoff_enabled;
        let msg = format!(
            "Auto-handoff on rotate: {}",
            if self.conversation.auto_handoff_enabled {
                "on"
            } else {
                "off"
            }
        );
        self.emit_conversation().await;
        self.emit_status_message(msg).await;
    }

    async fn run_turn(&mut self, agent: Agent, prompt: String, incoming_role: Role) {
        // Record the user/handoff turn that triggered this generation.
        // (If this is a Handoff, the synthetic turn was already appended by apply_rotation.)
        if matches!(incoming_role, Role::User) {
            let t = super::conversation::Turn::new(
                agent,
                Role::User,
                prompt.clone(),
                TurnStatus::Complete,
            );
            self.conversation.append_turn(t);
            self.emit_conversation().await;
        }

        self.set_status(WorkerStatus::Submitting { agent }).await;
        let streaming_id = self
            .conversation
            .start_streaming_turn(agent, Role::Assistant);
        self.emit_conversation().await;

        let (ev_tx, mut ev_rx) = mpsc::channel::<BackendEvent>(256);
        let input = BackendInput {
            prompt,
            conversation: self.conversation.clone(),
            model_override: match agent {
                Agent::Claude => self.cfg.claude_model.clone(),
                _ => None,
            },
        };

        // Run the backend in its own task so the UI can drain events as they arrive,
        // not in a burst after the turn completes. This is the critical fix for
        // "(waiting for first token…)" appearing to hang on long responses.
        let claude = self.claude.clone();
        let codex = self.codex.clone();
        let gpt = self.gpt.clone();
        let send_task = tokio::spawn(async move {
            match agent {
                Agent::Claude => claude.send(input, ev_tx).await,
                Agent::Codex => codex.send(input, ev_tx).await,
                Agent::Gpt => gpt.send(input, ev_tx).await,
            }
        });

        let mut streaming_started = false;
        while let Some(event) = ev_rx.recv().await {
            // Log every backend event (without potentially-huge delta bodies) for post-hoc debug.
            self.log.write("backend_event", &describe_event(&event));
            match event {
                BackendEvent::Started { .. } => {
                    self.set_status(WorkerStatus::Streaming { agent }).await;
                    streaming_started = true;
                }
                BackendEvent::TextDelta { text, .. } => {
                    if !streaming_started {
                        self.set_status(WorkerStatus::Streaming { agent }).await;
                        streaming_started = true;
                    }
                    self.conversation.extend_streaming_turn(streaming_id, &text);
                    self.emit_conversation().await;
                }
                BackendEvent::SessionUpdated { agent: a, id } => {
                    self.conversation
                        .sessions
                        .set_session_id_for(a, Some(id.clone()));
                    self.emit_conversation().await;
                }
                BackendEvent::Status { message, .. } => {
                    self.emit_status_message(message).await;
                }
                BackendEvent::Finished { .. } => {
                    // Finalisation handled via send_task.await below.
                }
                BackendEvent::Error { message, .. } => {
                    self.emit_status_message(message).await;
                }
            }
        }

        let send_result = match send_task.await {
            Ok(inner) => inner,
            Err(join_err) => Err(anyhow::anyhow!("backend task panicked: {join_err}")),
        };

        match send_result {
            Ok(_result) => {
                self.conversation
                    .finalise_turn(streaming_id, TurnStatus::Complete);
                self.emit_conversation().await;
                // Auto-compact after a successful turn so the next replay starts
                // from a smaller buffer. Best-effort: failures are logged, never
                // surfaced as a turn error.
                self.maybe_auto_compact().await;
                self.set_status(WorkerStatus::Idle).await;
            }
            Err(err) => {
                self.conversation
                    .finalise_turn(streaming_id, TurnStatus::Error);
                self.emit_conversation().await;
                self.set_status(WorkerStatus::Error {
                    message: err.to_string(),
                })
                .await;
                self.events
                    .send(WorkerEvent::Error(err.to_string()))
                    .await
                    .ok();
                // Return to idle after reporting so new commands can run.
                self.set_status(WorkerStatus::Idle).await;
            }
        }
    }

    async fn set_status(&mut self, status: WorkerStatus) {
        if self.status == status {
            return;
        }
        self.status = status.clone();
        self.events
            .send(WorkerEvent::StatusChanged(status))
            .await
            .ok();
    }

    async fn emit_conversation(&self) {
        if let Err(err) = self.store.save(&self.conversation) {
            tracing::warn!(%err, "failed to persist conversation snapshot");
        }
        self.log.write(
            "conversation_snapshot",
            &serde_json::json!({
                "turns": self.conversation.turns.len(),
                "active_agent": self.conversation.active_agent.label(),
                "last_turn_status": self.conversation.turns.last().map(|t| format!("{:?}", t.status)),
                "last_turn_len": self.conversation.turns.last().map(|t| t.content.len()),
                "claude_session_id": self.conversation.sessions.claude_session_id,
                "codex_thread_id": self.conversation.sessions.codex_thread_id,
            }),
        );
        self.events
            .send(WorkerEvent::ConversationUpdated(self.conversation.clone()))
            .await
            .ok();
    }

    async fn emit_status_message(&self, message: String) {
        self.log.write("status_message", &message);
        self.events
            .send(WorkerEvent::StatusMessage(message))
            .await
            .ok();
    }
}

fn describe_event(ev: &BackendEvent) -> serde_json::Value {
    match ev {
        BackendEvent::Started { agent } => {
            serde_json::json!({"type": "started", "agent": agent.label()})
        }
        BackendEvent::TextDelta { agent, text } => serde_json::json!({
            "type": "text_delta",
            "agent": agent.label(),
            "bytes": text.len(),
            // Include the text verbatim — a chat log that doesn't record what the model said
            // is useless. If privacy matters the file is already local-only.
            "text": text,
        }),
        BackendEvent::SessionUpdated { agent, id } => serde_json::json!({
            "type": "session_updated",
            "agent": agent.label(),
            "id": id,
        }),
        BackendEvent::Status { agent, message } => serde_json::json!({
            "type": "status",
            "agent": agent.label(),
            "message": message,
        }),
        BackendEvent::Finished { agent, final_text } => serde_json::json!({
            "type": "finished",
            "agent": agent.label(),
            "bytes": final_text.len(),
        }),
        BackendEvent::Error { agent, message } => serde_json::json!({
            "type": "error",
            "agent": agent.label(),
            "message": message,
        }),
    }
}

// Tiny helper to avoid test_name conflicts with existing tests.
#[allow(dead_code)]
fn _noop(_: Uuid) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_template_substitutes_prev_and_content() {
        let out = apply_handoff_template(DEFAULT_HANDOFF_TEMPLATE, Agent::Claude, "it works");
        assert!(out.contains("Claude"));
        assert!(out.contains("it works"));
    }
}

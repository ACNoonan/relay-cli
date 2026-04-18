//! Domain model for the relay chat TUI.
//!
//! Phase 0 capability-spike findings (2026-04-18) captured inline:
//!
//! Claude CLI (2.1.113) with `--bare --output-format stream-json --verbose --include-partial-messages`
//! emits one JSONL event per line:
//!   - `system` / `init` → contains `session_id` (UUID, resumable via `--resume`).
//!   - `stream_event` wrapping `content_block_delta` with `delta.type == "text_delta"` and `delta.text`.
//!   - `assistant` event with the full message content at block close.
//!   - `result` with `subtype: success|error`, `result` (final text), `total_cost_usd`, `stop_reason`.
//!
//! Codex CLI (0.118.0) with `codex exec --json` / `codex exec resume <id> "..."`:
//!   - `thread.started` → contains `thread_id` (UUID, resumable).
//!   - `turn.started`
//!   - `item.completed` with `item.type == "agent_message"` and `item.text` — delivered atomically,
//!     not as incremental text deltas (so the backend will emit a single `TextDelta` per item).
//!   - `turn.completed` with `usage.{input_tokens, cached_input_tokens, output_tokens}`.
//!   - On failure: `error { message }` + `turn.failed { error }`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which model the user is talking to. The ring order is defined by `Agent::next`/`Agent::prev`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Gpt,
    Codex,
}

impl Agent {
    pub const RING: [Agent; 3] = [Agent::Claude, Agent::Gpt, Agent::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Gpt => "GPT",
            Agent::Codex => "Codex",
        }
    }

    pub fn next(self) -> Agent {
        match self {
            Agent::Claude => Agent::Gpt,
            Agent::Gpt => Agent::Codex,
            Agent::Codex => Agent::Claude,
        }
    }

    pub fn prev(self) -> Agent {
        match self {
            Agent::Claude => Agent::Codex,
            Agent::Gpt => Agent::Claude,
            Agent::Codex => Agent::Gpt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Handoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    Complete,
    Streaming,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    pub agent: Agent,
    pub role: Role,
    pub content: String,
    pub ts: DateTime<Utc>,
    pub status: TurnStatus,
}

impl Turn {
    pub fn new(agent: Agent, role: Role, content: impl Into<String>, status: TurnStatus) -> Self {
        Self {
            id: Uuid::new_v4(),
            agent,
            role,
            content: content.into(),
            ts: Utc::now(),
            status,
        }
    }
}

/// Per-agent session continuity. Claude uses a resume session id; Codex uses a thread id;
/// GPT has no server-side session so we replay the local history each turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSessionState {
    pub claude_session_id: Option<String>,
    pub codex_thread_id: Option<String>,
}

impl AgentSessionState {
    pub fn clear(&mut self) {
        self.claude_session_id = None;
        self.codex_thread_id = None;
    }

    pub fn session_id_for(&self, agent: Agent) -> Option<&str> {
        match agent {
            Agent::Claude => self.claude_session_id.as_deref(),
            Agent::Codex => self.codex_thread_id.as_deref(),
            Agent::Gpt => None,
        }
    }

    pub fn set_session_id_for(&mut self, agent: Agent, id: Option<String>) {
        match agent {
            Agent::Claude => self.claude_session_id = id,
            Agent::Codex => self.codex_thread_id = id,
            Agent::Gpt => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub turns: Vec<Turn>,
    pub active_agent: Agent,
    pub sessions: AgentSessionState,
    pub auto_handoff_enabled: bool,
    /// Optional rolling summary, to be populated by a future compaction pass.
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Conversation {
    pub fn new(active_agent: Agent, auto_handoff_enabled: bool) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            turns: Vec::new(),
            active_agent,
            sessions: AgentSessionState::default(),
            auto_handoff_enabled,
            summary: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn append_turn(&mut self, turn: Turn) -> &Turn {
        self.turns.push(turn);
        self.updated_at = Utc::now();
        self.turns.last().expect("just pushed")
    }

    /// Append a streaming placeholder turn and return its id. Callers append deltas via
    /// `extend_streaming_turn` and finalise via `finalise_turn`.
    pub fn start_streaming_turn(&mut self, agent: Agent, role: Role) -> Uuid {
        let turn = Turn::new(agent, role, String::new(), TurnStatus::Streaming);
        let id = turn.id;
        self.append_turn(turn);
        id
    }

    pub fn extend_streaming_turn(&mut self, turn_id: Uuid, delta: &str) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == turn_id) {
            turn.content.push_str(delta);
            self.updated_at = Utc::now();
        }
    }

    pub fn finalise_turn(&mut self, turn_id: Uuid, status: TurnStatus) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == turn_id) {
            turn.status = status;
            self.updated_at = Utc::now();
        }
    }

    /// Overwrite content on an existing turn (used when the backend delivers one atomic
    /// payload — e.g. Codex `item.completed` — rather than incremental deltas).
    pub fn replace_turn_content(&mut self, turn_id: Uuid, content: impl Into<String>) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == turn_id) {
            turn.content = content.into();
            self.updated_at = Utc::now();
        }
    }

    pub fn last_assistant_turn(&self) -> Option<&Turn> {
        self.turns
            .iter()
            .rev()
            .find(|t| t.role == Role::Assistant && t.status == TurnStatus::Complete)
    }

    pub fn clear(&mut self) {
        self.turns.clear();
        self.sessions.clear();
        self.summary = None;
        self.updated_at = Utc::now();
    }

    pub fn rotate_next(&mut self) {
        self.active_agent = self.active_agent.next();
        self.updated_at = Utc::now();
    }

    pub fn rotate_prev(&mut self) {
        self.active_agent = self.active_agent.prev();
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_forward_is_three_cycle() {
        assert_eq!(Agent::Claude.next(), Agent::Gpt);
        assert_eq!(Agent::Gpt.next(), Agent::Codex);
        assert_eq!(Agent::Codex.next(), Agent::Claude);
    }

    #[test]
    fn ring_backward_is_inverse_of_forward() {
        for a in Agent::RING {
            assert_eq!(a.next().prev(), a);
            assert_eq!(a.prev().next(), a);
        }
    }

    #[test]
    fn streaming_turn_accumulates_deltas() {
        let mut c = Conversation::new(Agent::Claude, true);
        let id = c.start_streaming_turn(Agent::Claude, Role::Assistant);
        c.extend_streaming_turn(id, "hello ");
        c.extend_streaming_turn(id, "world");
        c.finalise_turn(id, TurnStatus::Complete);
        let t = c.last_assistant_turn().unwrap();
        assert_eq!(t.content, "hello world");
        assert_eq!(t.status, TurnStatus::Complete);
    }

    #[test]
    fn session_state_is_per_agent() {
        let mut s = AgentSessionState::default();
        s.set_session_id_for(Agent::Claude, Some("c-1".into()));
        s.set_session_id_for(Agent::Codex, Some("x-1".into()));
        assert_eq!(s.session_id_for(Agent::Claude), Some("c-1"));
        assert_eq!(s.session_id_for(Agent::Codex), Some("x-1"));
        assert_eq!(s.session_id_for(Agent::Gpt), None);
        s.clear();
        assert!(s.claude_session_id.is_none() && s.codex_thread_id.is_none());
    }
}

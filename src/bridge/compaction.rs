//! GPT replay-buffer compaction.
//!
//! ## Why this exists
//!
//! Per relay's README: *"GPT has no server-side session, so its history is
//! replayed locally each turn."* That means the on-the-wire `messages` array
//! grows unboundedly across long multi-agent sessions — every turn pays for
//! every preceding turn in input tokens, and eventually exceeds the model's
//! context window.
//!
//! Claude (`--resume`) and Codex (`exec resume <id>`) handle continuity
//! server-side, so they are **explicitly out of scope** here. This module only
//! looks at and rewrites the conversation buffer that `OpenAiBackend` replays.
//!
//! ## Strategy (mirrors pi-mono's `coding-agent/src/core/compaction/compaction.ts`)
//!
//! 1. Estimate replay-buffer size via a `chars / 4` heuristic on every text
//!    field (cheap, no tokenizer dep).
//! 2. When the estimate exceeds `CompactionConfig::trigger_tokens`, walk
//!    backwards from the newest turn until at least `keep_recent_tokens` worth
//!    of history is preserved.
//! 3. Send everything older than that cut point to GPT with a
//!    summarization-only system prompt; the response replaces those turns
//!    with a single synthetic summary turn (`Turn::new_summary`) at the head
//!    of the buffer.
//!
//! pi's prompt is geared toward a single-agent coding loop. Relay's variant
//! preserves the same structured-summary skeleton (Goal / Progress / Decisions
//! / Next Steps / Critical Context) but reframes it for *multi-agent*
//! conversations across Claude, GPT, and Codex.
//!
//! ## Test seam
//!
//! [`Summarizer`] is a small async trait so tests can mock the LLM call. The
//! real implementation, [`OpenAiSummarizer`], shares the same key/host the
//! main GPT backend uses.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use super::conversation::{Conversation, Role, Turn, TurnStatus};

// ============================================================================
// Configuration
// ============================================================================

/// Compaction tuning. Loaded from `[bridge.compaction]` in the harness config
/// (with env-var overrides for ops convenience), and falls back to
/// [`CompactionConfig::default`] when absent.
#[derive(Debug, Clone, Copy)]
pub struct CompactionConfig {
    /// Master switch. Auto-compaction is silently disabled when this is false;
    /// `compact_gpt_history()` (the manual `/compact` entry point) still runs.
    pub auto_enabled: bool,
    /// Estimated-token threshold at which auto-compaction kicks in. Default
    /// **32_000** — chosen as roughly a quarter of GPT-5.4's headline 128k
    /// window: large enough that we don't summarize away useful recency on
    /// short sessions, small enough that summarization itself fits comfortably
    /// inside one round-trip and leaves headroom for the next reply.
    pub trigger_tokens: usize,
    /// How much recent history (in estimated tokens) to retain *un*-summarized.
    /// Default **8_000** mirrors pi-mono's `keepRecentTokens` order of
    /// magnitude scaled for relay's smaller average turn size.
    pub keep_recent_tokens: usize,
    /// Hard floor on how many turns we leave alone at the tail of the buffer.
    /// Belt-and-suspenders alongside `keep_recent_tokens` so very long single
    /// turns can't push the keep-set down to zero.
    pub min_keep_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            auto_enabled: true,
            trigger_tokens: 32_000,
            keep_recent_tokens: 8_000,
            min_keep_turns: 4,
        }
    }
}

impl CompactionConfig {
    /// Build a runtime `CompactionConfig` from the on-disk
    /// [`crate::config::CompactionConfigToml`]. Field names are kept 1:1 so
    /// adding a tuneable means adding it in both places, deliberately — the
    /// on-disk type is the user-visible contract.
    pub fn from_toml(t: &crate::config::CompactionConfigToml) -> Self {
        Self {
            auto_enabled: t.auto_enabled,
            trigger_tokens: t.trigger_tokens,
            keep_recent_tokens: t.keep_recent_tokens,
            min_keep_turns: t.min_keep_turns,
        }
    }

    /// Apply environment-variable overrides (handy for ops + tests). All vars
    /// are optional; bad values are ignored rather than failing startup.
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(v) = std::env::var("RELAY_COMPACTION_TRIGGER_TOKENS") {
            if let Ok(n) = v.parse::<usize>() {
                self.trigger_tokens = n;
            }
        }
        if let Ok(v) = std::env::var("RELAY_COMPACTION_KEEP_TOKENS") {
            if let Ok(n) = v.parse::<usize>() {
                self.keep_recent_tokens = n;
            }
        }
        if let Ok(v) = std::env::var("RELAY_COMPACTION_AUTO") {
            self.auto_enabled = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        self
    }
}

// ============================================================================
// Token estimation
// ============================================================================

/// Estimate tokens in a single string using the chars/4 heuristic. Conservative
/// (i.e. tends to overestimate), which is the safe direction for a budget
/// trigger.
pub fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Estimate tokens in a single turn. Includes a small fixed overhead for the
/// JSON envelope (`{"role":..,"content":..}`) so summed turn estimates stay
/// close to what we actually put on the wire.
pub fn estimate_turn_tokens(turn: &Turn) -> usize {
    const ENVELOPE_TOKENS: usize = 4;
    if turn.status == TurnStatus::Error {
        return 0;
    }
    estimate_text_tokens(&turn.content) + ENVELOPE_TOKENS
}

/// Estimate the total replay-buffer token count for the GPT path: the system
/// prompt plus all non-error turns. This is the value compared against
/// [`CompactionConfig::trigger_tokens`].
pub fn estimate_replay_tokens(system_prompt: &str, turns: &[Turn]) -> usize {
    let mut total = estimate_text_tokens(system_prompt) + 4;
    for t in turns {
        total += estimate_turn_tokens(t);
    }
    total
}

// ============================================================================
// Cut-point planning
// ============================================================================

/// Outcome of inspecting a conversation against a [`CompactionConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionPlan {
    /// Estimated tokens are under the trigger; no work to do.
    Skip { estimated_tokens: usize },
    /// We should summarize `turns_to_summarize` (the prefix of the buffer) and
    /// keep the remaining tail intact.
    Compact {
        estimated_tokens: usize,
        cut_index: usize,
        turns_to_summarize: usize,
    },
}

/// Walk backwards from the newest turn, accumulating estimated tokens until we
/// have at least `keep_recent_tokens` worth of recent context. The resulting
/// `cut_index` is the first turn that will be **kept**; everything before it
/// gets summarized.
///
/// Refuses to cut when fewer than `min_keep_turns` would remain.
pub fn plan_compaction(
    system_prompt: &str,
    turns: &[Turn],
    cfg: &CompactionConfig,
) -> CompactionPlan {
    let estimated_tokens = estimate_replay_tokens(system_prompt, turns);
    if estimated_tokens <= cfg.trigger_tokens {
        return CompactionPlan::Skip { estimated_tokens };
    }
    if turns.len() <= cfg.min_keep_turns {
        return CompactionPlan::Skip { estimated_tokens };
    }

    // Walk back accumulating until we cross keep_recent_tokens.
    let mut acc = 0usize;
    let mut cut_index = turns.len();
    for (i, t) in turns.iter().enumerate().rev() {
        acc += estimate_turn_tokens(t);
        cut_index = i;
        if acc >= cfg.keep_recent_tokens {
            break;
        }
    }

    // Avoid cutting the buffer to fewer than min_keep_turns kept.
    let kept = turns.len() - cut_index;
    if kept < cfg.min_keep_turns {
        cut_index = turns.len().saturating_sub(cfg.min_keep_turns);
    }

    // If after clamping we'd summarize zero turns, skip.
    if cut_index == 0 {
        return CompactionPlan::Skip { estimated_tokens };
    }

    // If the boundary lands right after an existing summary turn, fold it back
    // in: we want exactly one summary turn at the head. (Only one should ever
    // exist, but loop defensively.)
    while cut_index > 0 && turns[cut_index - 1].is_summary() {
        cut_index -= 1;
    }

    CompactionPlan::Compact {
        estimated_tokens,
        cut_index,
        turns_to_summarize: cut_index,
    }
}

// ============================================================================
// Summarization prompt
// ============================================================================

/// System prompt for the summarization request. Adapted from pi-mono's
/// `SUMMARIZATION_SYSTEM_PROMPT` but framed for relay's multi-agent setting.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str =
    "You are a context summarization assistant for a multi-agent conversation between a human user \
     and AI assistants (Claude Code, GPT, and Codex collaborating in a single chat). Read the \
     conversation provided and produce a structured summary in the exact format requested. \
     Do NOT continue the conversation. Do NOT respond to any questions in it. ONLY output the \
     structured summary.";

/// Instructions appended after the serialized conversation.
pub const SUMMARIZATION_INSTRUCTIONS: &str = "\
The messages above are a multi-agent conversation to summarize. Produce a structured context \
checkpoint another LLM will use to continue collaborating with the user.

Use this EXACT format:

## Goal
[What is the user trying to accomplish across this conversation?]

## Constraints & Preferences
- [Constraints, preferences, or requirements voiced by the user]
- [Or \"(none)\" if none were stated]

## Per-Agent Contributions
- **Claude**: [What Claude has built / proposed / decided. Or \"(no turns)\".]
- **GPT**: [What GPT has reviewed / suggested / decided. Or \"(no turns)\".]
- **Codex**: [What Codex has implemented / verified / decided. Or \"(no turns)\".]

## Progress
### Done
- [x] [Completed work]

### In Progress
- [ ] [Current work]

### Blocked
- [Open blockers, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Critical Context
- [Files, identifiers, error messages, links the next agent needs verbatim]
- [Or \"(none)\"]

## Next Steps
1. [Ordered list of what should happen next]

Keep each section concise. Preserve exact file paths, function names, error messages, and command \
strings. Do not invent details not present above.";

/// Serialize a slice of turns as plain text for the summarizer prompt. Avoids
/// re-using the on-wire JSON shape so the model is less tempted to continue
/// the conversation.
pub fn serialize_turns_for_summary(turns: &[Turn]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for t in turns {
        if t.status == TurnStatus::Error {
            continue;
        }
        let label = if t.is_summary() {
            format!(
                "[Prior summary covering {} turns]",
                t.summarized_turn_count.unwrap_or_default()
            )
        } else {
            match t.role {
                Role::User => "[User]".to_string(),
                Role::Handoff => format!("[Handoff -> {}]", t.agent.label()),
                Role::Assistant => format!("[{}]", t.agent.label()),
                Role::System => "[System]".to_string(),
            }
        };
        // Truncate very long single turns so the summarization request stays
        // within budget. Mirrors pi-mono's `TOOL_RESULT_MAX_CHARS = 2000` cap.
        const PER_TURN_MAX_CHARS: usize = 6_000;
        let body = if t.content.chars().count() > PER_TURN_MAX_CHARS {
            let kept: String = t.content.chars().take(PER_TURN_MAX_CHARS).collect();
            let dropped = t.content.chars().count() - PER_TURN_MAX_CHARS;
            format!("{kept}\n\n[... {dropped} characters truncated for summary]")
        } else {
            t.content.clone()
        };
        let _ = writeln!(out, "{label}: {body}\n");
    }
    out
}

// ============================================================================
// Summarizer abstraction (test seam)
// ============================================================================

/// Anything that can answer one summarization request. Abstracted so tests can
/// supply a canned response without hitting the network.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, system_prompt: &str, user_prompt: &str) -> Result<String>;
}

/// Production summarizer: a single non-streaming call to OpenAI Chat
/// Completions on the configured model.
pub struct OpenAiSummarizer {
    pub client: Client,
    pub api_key: String,
    pub model: String,
}

impl OpenAiSummarizer {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set; required for GPT replay-buffer compaction")?;
        Ok(Self {
            client: Client::new(),
            api_key,
            model: model.into(),
        })
    }
}

#[async_trait]
impl Summarizer for OpenAiSummarizer {
    async fn summarize(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let body = json!({
            "model": self.model,
            "stream": false,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
        });
        let resp = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("calling OpenAI Chat Completions for summarization")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI summarization error ({status}): {text}");
        }

        let v: Value = resp
            .json()
            .await
            .context("parsing OpenAI summarization response")?;
        let summary = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .context("OpenAI summarization response missing choices[0].message.content")?;
        Ok(summary)
    }
}

// ============================================================================
// Driver: compact a Conversation using a Summarizer
// ============================================================================

/// What `compact_conversation` did.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub turns_summarized: usize,
    pub turns_remaining: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub summary: String,
}

/// In-memory compaction. Mutates `conv` in place: replaces the prefix selected
/// by [`plan_compaction`] with a single summary turn, then mirrors the latest
/// summary text into `conv.summary`.
///
/// Returns `Ok(None)` when the plan said `Skip`. Errors only when the
/// summarizer itself fails — the conversation is left untouched in that case.
pub async fn compact_conversation(
    conv: &mut Conversation,
    system_prompt: &str,
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
) -> Result<Option<CompactionResult>> {
    let plan = plan_compaction(system_prompt, &conv.turns, cfg);
    let (cut_index, turns_to_summarize, estimated_tokens_before) = match plan {
        CompactionPlan::Skip { .. } => return Ok(None),
        CompactionPlan::Compact {
            cut_index,
            turns_to_summarize,
            estimated_tokens,
        } => (cut_index, turns_to_summarize, estimated_tokens),
    };

    // Build the user-side prompt: the serialized prior conversation, followed
    // by the structured-format instructions. We deliberately wrap the
    // conversation in tags so the model treats it as data, not as a thread to
    // continue.
    let serialized = serialize_turns_for_summary(&conv.turns[..cut_index]);
    let user_prompt =
        format!("<conversation>\n{serialized}</conversation>\n\n{SUMMARIZATION_INSTRUCTIONS}",);

    let summary_text = summarizer
        .summarize(SUMMARIZATION_SYSTEM_PROMPT, &user_prompt)
        .await
        .context("generating GPT replay-buffer summary")?;

    // Splice: drop the summarized prefix, prepend a single summary turn.
    let kept_tail: Vec<Turn> = conv.turns.drain(cut_index..).collect();
    conv.turns.clear();
    conv.turns
        .push(Turn::new_summary(summary_text.clone(), turns_to_summarize));
    conv.turns.extend(kept_tail);
    conv.summary = Some(summary_text.clone());
    conv.updated_at = chrono::Utc::now();

    let estimated_tokens_after = estimate_replay_tokens(system_prompt, &conv.turns);

    Ok(Some(CompactionResult {
        turns_summarized: turns_to_summarize,
        turns_remaining: conv.turns.len(),
        estimated_tokens_before,
        estimated_tokens_after,
        summary: summary_text,
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::conversation::{Agent, Conversation, Role, TurnStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn make_turn(role: Role, agent: Agent, content: &str) -> Turn {
        Turn::new(agent, role, content, TurnStatus::Complete)
    }

    fn long_text(n: usize) -> String {
        // Every char is one byte → chars/4 tokens.
        "x".repeat(n)
    }

    /// Counts calls + returns a canned summary; lets us assert exactly one
    /// LLM call per `compact_conversation` invocation.
    struct MockSummarizer {
        text: String,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Summarizer for MockSummarizer {
        async fn summarize(&self, _sys: &str, _user: &str) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.text.clone())
        }
    }

    #[test]
    fn estimate_replay_tokens_is_chars_over_four_ish() {
        let turn = make_turn(Role::User, Agent::Gpt, &long_text(400));
        // 400 chars ≈ 100 tokens + envelope.
        let est = estimate_turn_tokens(&turn);
        assert!((100..=110).contains(&est), "got {est}");
    }

    #[test]
    fn plan_skips_when_under_trigger() {
        let cfg = CompactionConfig {
            trigger_tokens: 1_000,
            keep_recent_tokens: 200,
            min_keep_turns: 2,
            auto_enabled: true,
        };
        let turns = vec![
            make_turn(Role::User, Agent::Gpt, "hi"),
            make_turn(Role::Assistant, Agent::Gpt, "hello"),
        ];
        let plan = plan_compaction("system", &turns, &cfg);
        assert!(matches!(plan, CompactionPlan::Skip { .. }));
    }

    #[test]
    fn plan_compacts_when_over_trigger() {
        let cfg = CompactionConfig {
            trigger_tokens: 100,
            keep_recent_tokens: 50,
            min_keep_turns: 2,
            auto_enabled: true,
        };
        // 6 turns of ~100 tokens each => well above trigger.
        let turns: Vec<Turn> = (0..6)
            .map(|i| {
                make_turn(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    Agent::Gpt,
                    &long_text(400),
                )
            })
            .collect();
        let plan = plan_compaction("system", &turns, &cfg);
        match plan {
            CompactionPlan::Compact {
                cut_index,
                turns_to_summarize,
                ..
            } => {
                assert!(cut_index > 0 && cut_index < turns.len());
                assert_eq!(turns_to_summarize, cut_index);
                assert!(turns.len() - cut_index >= cfg.min_keep_turns);
            }
            other => panic!("expected Compact plan, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_conversation_replaces_prefix_with_one_summary_turn() {
        let cfg = CompactionConfig {
            trigger_tokens: 100,
            keep_recent_tokens: 60,
            min_keep_turns: 2,
            auto_enabled: true,
        };
        let mut conv = Conversation::new(Agent::Gpt, true);
        for i in 0..6 {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            conv.turns
                .push(make_turn(role, Agent::Gpt, &long_text(400)));
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let summ = MockSummarizer {
            text: "## Goal\nfake summary".into(),
            calls: calls.clone(),
        };
        let res = compact_conversation(&mut conv, "system", &cfg, &summ)
            .await
            .expect("compaction should succeed")
            .expect("plan should not have skipped");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(res.turns_summarized >= 1);
        assert!(res.estimated_tokens_after < res.estimated_tokens_before);
        // The new head must be exactly one summary turn carrying the count.
        assert!(conv.turns[0].is_summary());
        assert_eq!(
            conv.turns[0].summarized_turn_count,
            Some(res.turns_summarized)
        );
        assert_eq!(conv.summary.as_deref(), Some("## Goal\nfake summary"));
        // Tail length == original 6 - summarized + 1 summary turn.
        assert_eq!(conv.turns.len(), 6 - res.turns_summarized + 1);
    }

    #[tokio::test]
    async fn compact_conversation_skips_when_under_threshold() {
        let cfg = CompactionConfig {
            trigger_tokens: 1_000_000,
            ..CompactionConfig::default()
        };
        let mut conv = Conversation::new(Agent::Gpt, true);
        conv.turns.push(make_turn(Role::User, Agent::Gpt, "hi"));
        let calls = Arc::new(AtomicUsize::new(0));
        let summ = MockSummarizer {
            text: "should-not-be-called".into(),
            calls: calls.clone(),
        };
        let res = compact_conversation(&mut conv, "system", &cfg, &summ)
            .await
            .expect("ok");
        assert!(res.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn second_compaction_folds_in_prior_summary() {
        let cfg = CompactionConfig {
            trigger_tokens: 100,
            keep_recent_tokens: 60,
            min_keep_turns: 2,
            auto_enabled: true,
        };
        let mut conv = Conversation::new(Agent::Gpt, true);
        // Pre-existing summary turn at the head.
        conv.turns.push(Turn::new_summary("prior summary", 5));
        // Plus enough fresh turns to push us back over the trigger.
        for i in 0..6 {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            conv.turns
                .push(make_turn(role, Agent::Gpt, &long_text(400)));
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let summ = MockSummarizer {
            text: "merged summary".into(),
            calls: calls.clone(),
        };
        compact_conversation(&mut conv, "system", &cfg, &summ)
            .await
            .expect("ok")
            .expect("should compact");

        // Exactly one summary turn at the head.
        let summary_turns = conv.turns.iter().filter(|t| t.is_summary()).count();
        assert_eq!(summary_turns, 1);
        assert_eq!(conv.turns[0].content, "merged summary");
    }
}

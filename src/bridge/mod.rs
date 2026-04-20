//! Chat bridge: single-pane multi-agent TUI driving Claude, GPT, and Codex.
//!
//! Entry points:
//!  - [`run_chat`] — invoked by `relay chat`, the supported surface.
//!  - [`run`] — invoked by `relay bridge`, a deprecated alias that maps to `run_chat`.

pub mod agent;
mod chat_log;
mod claude_backend;
mod codex_backend;
pub mod compaction;
pub mod conversation;
mod openai_client;
pub(crate) mod persist;
pub mod session_picker;
mod slash;
mod tui_chat;
pub mod worker;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

use crate::config::HarnessConfig;
use crate::storage::Storage;
use conversation::{Agent, Conversation};
use persist::ConversationStore;
use worker::{WorkerConfig, DEFAULT_GPT_SYSTEM_PROMPT, DEFAULT_HANDOFF_TEMPLATE};

/// Options accepted by `relay bridge`. The pre-refactor split-pane bridge had more
/// fields; we keep this struct for CLI back-compat and map it onto `WorkerConfig`.
#[derive(Debug, Clone, Default)]
pub struct BridgeOptions {
    pub prompt: String,
    pub claude_model: Option<String>,
    pub claude_binary: String,
    pub gpt_model: String,
    pub reviewer_prompt_file: Option<String>,
    pub resume_session_id: Option<String>,
}

pub async fn run(options: BridgeOptions) -> Result<()> {
    let system_prompt = match options.reviewer_prompt_file.as_deref() {
        Some(path) => std::fs::read_to_string(path)?,
        None => DEFAULT_GPT_SYSTEM_PROMPT.to_string(),
    };

    let cfg = WorkerConfig {
        claude_binary: if options.claude_binary.is_empty() {
            "claude".into()
        } else {
            options.claude_binary.clone()
        },
        codex_binary: "codex".into(),
        claude_model: options.claude_model.clone(),
        gpt_model: if options.gpt_model.is_empty() {
            "gpt-5.4".into()
        } else {
            options.gpt_model.clone()
        },
        gpt_system_prompt: system_prompt,
        initial_agent: Agent::Claude,
        auto_handoff_enabled: true,
        handoff_template: DEFAULT_HANDOFF_TEMPLATE.into(),
        harness_root: Some(camino::Utf8PathBuf::from(".agent-harness")),
        resume_conversation: None,
        compaction: compaction::CompactionConfig::default().with_env_overrides(),
    };

    // NOTE: `resume_session_id` is a Claude-only flag from the legacy split-pane bridge.
    // The new chat TUI treats conversation resume via `relay chat --resume <conv-uuid>`
    // (Phase 8). For now, if the caller passes `resume_session_id` we silently prefer a
    // fresh conversation — rehydration will land with persistence.
    let _ = options.resume_session_id;

    let initial_prompt = if options.prompt.trim().is_empty() {
        None
    } else {
        Some(options.prompt.clone())
    };

    tui_chat::run(cfg, initial_prompt).await
}

#[derive(Debug, Clone)]
pub struct ChatOptions {
    pub prompt: Option<String>,
    pub start_with: Agent,
    pub claude_model: Option<String>,
    pub claude_binary: String,
    pub codex_binary: String,
    pub gpt_model: String,
    pub system_prompt_file: Option<String>,
    pub auto_handoff: bool,
    pub resume_conversation_id: Option<uuid::Uuid>,
    /// Set by `--new`: skip the auto-opening fuzzy picker and start fresh
    /// even when prior conversations exist.
    pub skip_picker: bool,
    pub harness_root: Utf8PathBuf,
}

pub async fn run_chat(opts: ChatOptions) -> Result<()> {
    let gpt_system_prompt = match opts.system_prompt_file.as_deref() {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading system prompt file {path}"))?,
        None => DEFAULT_GPT_SYSTEM_PROMPT.to_string(),
    };

    // If the caller didn't pre-pick a conversation and didn't pass `--new`,
    // open the fuzzy picker. Returning `None` from the picker (cancel or
    // explicit "New conversation" sentinel) means start fresh.
    let resolved_resume_id = match opts.resume_conversation_id {
        Some(id) => Some(id),
        None if opts.skip_picker => None,
        None => session_picker::pick_session(&opts.harness_root).await?,
    };

    let resume_conversation = match resolved_resume_id {
        Some(id) => {
            let store = ConversationStore::open(Some(opts.harness_root.clone()));
            if !store.is_enabled() {
                anyhow::bail!(
                    "Harness not initialised at {}; cannot resume a conversation. Run `relay init` first.",
                    opts.harness_root
                );
            }
            let conv: Conversation = store.load(id)?;
            Some(conv)
        }
        None => None,
    };

    let initial_agent = resume_conversation
        .as_ref()
        .map(|c| c.active_agent)
        .unwrap_or(opts.start_with);
    let auto_handoff_enabled = resume_conversation
        .as_ref()
        .map(|c| c.auto_handoff_enabled)
        .unwrap_or(opts.auto_handoff);

    // Pull `[bridge.compaction]` from the user's harness config when available;
    // fall back to defaults otherwise. Env vars take final precedence so ops
    // can override without editing the file.
    let compaction_cfg = {
        let storage = Storage::new(opts.harness_root.clone());
        let from_disk = if storage.is_initialized() {
            HarnessConfig::load(&storage.config_path()).ok()
        } else {
            None
        };
        let toml_view = from_disk
            .as_ref()
            .map(|h| h.bridge.compaction.clone())
            .unwrap_or_default();
        compaction::CompactionConfig::from_toml(&toml_view).with_env_overrides()
    };

    let cfg = WorkerConfig {
        claude_binary: opts.claude_binary,
        codex_binary: opts.codex_binary,
        claude_model: opts.claude_model,
        gpt_model: opts.gpt_model,
        gpt_system_prompt,
        initial_agent,
        auto_handoff_enabled,
        handoff_template: DEFAULT_HANDOFF_TEMPLATE.into(),
        harness_root: Some(opts.harness_root),
        resume_conversation,
        compaction: compaction_cfg,
    };

    tui_chat::run(cfg, opts.prompt).await
}

pub fn parse_start_with(s: &str) -> Agent {
    match s {
        "gpt" => Agent::Gpt,
        "codex" => Agent::Codex,
        _ => Agent::Claude,
    }
}

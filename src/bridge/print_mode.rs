//! Non-interactive `relay chat --print` mode.
//!
//! Drives a single multi-agent rotation against the same [`Worker`] state
//! machine the TUI uses, without any terminal UI. Subscribes to the worker's
//! [`EventBus`](super::events::EventBus) exactly like `tui_chat`, sends
//! `SendToActive` / `RotateTo` commands sequentially, and emits the result to
//! stdout in either plain text (default) or NDJSON (`--format json`).
//!
//! Shape:
//! 1. Start the worker with `initial_agent = rotation[0]`.
//! 2. Submit the initial prompt to the active agent. Wait for that turn to
//!    complete (status returns to Idle).
//! 3. For each subsequent agent in the rotation, rotate (auto-handoff =
//!    always on) and wait for the resulting turn to complete. If the next
//!    agent equals the current one we bypass `RotateTo` (which no-ops on
//!    self-rotate) and submit the handoff-templated prev content directly.
//! 4. On any error event, abort the rotation and exit non-zero.
//! 5. Send `WorkerCommand::Quit`, give the worker a brief grace period, and
//!    return.
//!
//! Conversation persistence is handled by the worker — a JSON file lands in
//! `.agent-harness/conversations/<uuid>/` so an interactive `relay chat
//! --resume <uuid>` can pick the result up later.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use serde::Serialize;
use tokio::sync::{broadcast::error::RecvError, mpsc};
use uuid::Uuid;

use super::compaction;
use super::conversation::{Agent, Conversation, Role, TurnStatus};
use super::worker::{
    apply_handoff_template, Worker, WorkerCommand, WorkerConfig, WorkerEvent, WorkerStatus,
    DEFAULT_GPT_SYSTEM_PROMPT, DEFAULT_HANDOFF_TEMPLATE,
};
use crate::config::HarnessConfig;
use crate::storage::Storage;

/// Output format for print mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintFormat {
    /// Final assistant message printed verbatim (followed by a single newline).
    Text,
    /// One JSON object per line (NDJSON), one line per significant event.
    Json,
}

impl PrintFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(anyhow!(
                "invalid --format `{other}` (expected `text` or `json`)"
            )),
        }
    }
}

/// All the knobs print mode needs. Mirrors the bits of [`super::ChatOptions`]
/// that don't depend on a terminal session.
#[derive(Debug, Clone)]
pub struct PrintModeOptions {
    pub initial_prompt: String,
    /// Ordered list of agents that will each take one turn.
    pub rotation: Vec<Agent>,
    pub format: PrintFormat,
    pub harness_dir: Utf8PathBuf,
    pub claude_model: Option<String>,
    pub gpt_model: Option<String>,
    pub claude_binary: Option<String>,
    pub codex_binary: Option<String>,
    pub system_prompt_file: Option<String>,
}

/// NDJSON event schema. Stable surface for tooling.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PrintEvent<'a> {
    /// Emitted before each agent's turn begins.
    TurnStart { agent: &'a str, seq: usize },
    /// Emitted after each agent's turn completes successfully.
    TurnEnd {
        agent: &'a str,
        seq: usize,
        content: &'a str,
    },
    /// Emitted on a backend or worker error. `agent` is the agent whose turn
    /// failed when known.
    Error {
        agent: Option<&'a str>,
        message: &'a str,
    },
    /// Final event of the run. `exit_code` is 0 on success, non-zero on failure.
    Done {
        conversation_id: String,
        exit_code: i32,
    },
}

/// Parse a comma-separated rotation list (e.g. `"claude,codex,gpt"`).
///
/// - Trims whitespace around each entry.
/// - Empty input → error.
/// - Any unknown agent name → error (the full input is included in the error
///   so CI logs are actionable).
pub fn parse_rotation(input: &str) -> Result<Vec<Agent>> {
    let mut out = Vec::new();
    for raw in input.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let agent = match name.to_ascii_lowercase().as_str() {
            "claude" => Agent::Claude,
            "gpt" => Agent::Gpt,
            "codex" => Agent::Codex,
            other => {
                return Err(anyhow!(
                    "unknown agent `{other}` in --rotation (expected one of: claude, gpt, codex)"
                ));
            }
        };
        out.push(agent);
    }
    if out.is_empty() {
        return Err(anyhow!(
            "--rotation cannot be empty (expected comma-separated list of: claude, gpt, codex)"
        ));
    }
    Ok(out)
}

/// Run a single multi-agent rotation non-interactively.
///
/// Returns the desired process exit code (0 on success, non-zero on failure).
pub async fn run_print(opts: PrintModeOptions) -> Result<i32> {
    if opts.rotation.is_empty() {
        return Err(anyhow!("rotation must contain at least one agent"));
    }

    let gpt_system_prompt = match opts.system_prompt_file.as_deref() {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading system prompt file {path}"))?,
        None => DEFAULT_GPT_SYSTEM_PROMPT.to_string(),
    };

    // Mirror the harness-config plumbing in `run_chat` so print mode honours
    // the same `[bridge.compaction]` overrides.
    let compaction_cfg = {
        let storage = Storage::new(opts.harness_dir.clone());
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
        claude_binary: opts
            .claude_binary
            .clone()
            .unwrap_or_else(|| "claude".to_string()),
        codex_binary: opts
            .codex_binary
            .clone()
            .unwrap_or_else(|| "codex".to_string()),
        claude_model: opts.claude_model.clone(),
        gpt_model: opts
            .gpt_model
            .clone()
            .unwrap_or_else(|| "gpt-5.4".to_string()),
        gpt_system_prompt,
        initial_agent: opts.rotation[0],
        // Auto-handoff is "always on in print mode" per spec; this also matches
        // what RotateTo's handoff_last_assistant flag drives turn-by-turn.
        auto_handoff_enabled: true,
        handoff_template: DEFAULT_HANDOFF_TEMPLATE.into(),
        harness_root: Some(opts.harness_dir.clone()),
        resume_conversation: None,
        compaction: compaction_cfg,
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>(64);
    let worker = Worker::new(cfg)?;
    // Subscribe BEFORE spawning the worker — `tokio::sync::broadcast` only
    // delivers events emitted *after* a receiver is created. Same constraint
    // `tui_chat::run` relies on.
    let mut ev_rx = worker.bus().subscribe();
    let worker_task = tokio::spawn(async move {
        if let Err(err) = worker.run(cmd_rx).await {
            tracing::error!(%err, "print-mode worker exited with error");
        }
    });

    // ---- Ctrl+C handling ----
    // First Ctrl+C: send Quit, return 130. We don't try to interrupt an
    // in-flight backend process — vendor CLIs own their own process trees and
    // killing them mid-stream can corrupt their session files. If the user
    // hits Ctrl+C twice, tokio's default behaviour aborts the process.
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        let cmd_tx_for_signal = cmd_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = cmd_tx_for_signal.send(WorkerCommand::Quit).await;
            }
        });
    }

    let outcome = drive_rotation(&opts, &cmd_tx, &mut ev_rx, cancel.clone()).await;

    // Best-effort clean shutdown. `Quit` may have been sent already by the
    // signal handler; double-send is harmless (the worker will already have
    // exited and the channel will return Err which we ignore).
    let _ = cmd_tx.send(WorkerCommand::Quit).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), worker_task).await;

    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        // Conventional exit code for SIGINT.
        return Ok(130);
    }

    // Render the final outcome to stdout per format.
    match outcome {
        Ok(success) => {
            match opts.format {
                PrintFormat::Text => {
                    // Final agent's content goes to stdout, nothing else.
                    println!("{}", success.final_content);
                }
                PrintFormat::Json => {
                    emit_json(&PrintEvent::Done {
                        conversation_id: success.conversation_id.to_string(),
                        exit_code: 0,
                    });
                }
            }
            Ok(0)
        }
        Err(failure) => {
            match opts.format {
                PrintFormat::Text => {
                    eprintln!("relay chat --print: {}", failure.message);
                }
                PrintFormat::Json => {
                    let agent_label = failure.agent.map(|a| a.label());
                    emit_json(&PrintEvent::Error {
                        agent: agent_label,
                        message: &failure.message,
                    });
                    emit_json(&PrintEvent::Done {
                        conversation_id: failure
                            .conversation_id
                            .map(|u| u.to_string())
                            .unwrap_or_default(),
                        exit_code: 1,
                    });
                }
            }
            Ok(1)
        }
    }
}

/// What a successful rotation produces.
struct PrintSuccess {
    final_content: String,
    conversation_id: Uuid,
}

/// What a failed rotation produces.
struct PrintFailure {
    message: String,
    agent: Option<Agent>,
    conversation_id: Option<Uuid>,
}

async fn drive_rotation(
    opts: &PrintModeOptions,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
    ev_rx: &mut tokio::sync::broadcast::Receiver<WorkerEvent>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::result::Result<PrintSuccess, PrintFailure> {
    let mut current_agent = opts.rotation[0];
    let mut last_conversation: Option<Conversation> = None;
    let mut last_assistant_content = String::new();

    for (seq, &target_agent) in opts.rotation.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(PrintFailure {
                message: "interrupted".into(),
                agent: Some(target_agent),
                conversation_id: last_conversation.as_ref().map(|c| c.id),
            });
        }

        if opts.format == PrintFormat::Json {
            emit_json(&PrintEvent::TurnStart {
                agent: target_agent.label(),
                seq,
            });
        }

        // Pick the right command for this turn.
        // - First turn: send the user's initial prompt to the (already-active) agent.
        // - Subsequent turns where target != current: ask the worker to rotate
        //   with auto-handoff. The worker fabricates the wrapped handoff prompt.
        // - Subsequent turns where target == current: rotation would be a
        //   no-op in the worker, so we apply the handoff template ourselves
        //   and send it as a regular user turn. This makes `--rotation
        //   claude,claude` behave the way users expect (two Claude turns,
        //   second seeded by the first).
        let cmd = if seq == 0 {
            WorkerCommand::SendToActive {
                prompt: opts.initial_prompt.clone(),
            }
        } else if target_agent != current_agent {
            WorkerCommand::RotateTo {
                agent: target_agent,
                handoff_last_assistant: true,
            }
        } else {
            let prompt = apply_handoff_template(
                DEFAULT_HANDOFF_TEMPLATE,
                current_agent,
                &last_assistant_content,
            );
            WorkerCommand::SendToActive { prompt }
        };

        if cmd_tx.send(cmd).await.is_err() {
            return Err(PrintFailure {
                message: "worker channel closed before turn could be submitted".into(),
                agent: Some(target_agent),
                conversation_id: last_conversation.as_ref().map(|c| c.id),
            });
        }

        // Wait for this turn to complete. We track conversation snapshots and
        // any error event the worker emits. The turn is "done" when status
        // returns to Idle after first leaving it.
        match wait_for_turn(ev_rx, cancel.clone()).await {
            TurnOutcome::Completed {
                conversation,
                error_message,
            } => {
                last_conversation = Some(conversation.clone());
                if let Some(message) = error_message {
                    return Err(PrintFailure {
                        message,
                        agent: Some(target_agent),
                        conversation_id: Some(conversation.id),
                    });
                }
                let final_turn = conversation.last_assistant_turn().cloned();
                let Some(turn) = final_turn else {
                    return Err(PrintFailure {
                        message: format!(
                            "agent {} produced no assistant turn",
                            target_agent.label()
                        ),
                        agent: Some(target_agent),
                        conversation_id: Some(conversation.id),
                    });
                };
                if !matches!(turn.status, TurnStatus::Complete) {
                    return Err(PrintFailure {
                        message: format!(
                            "agent {} did not complete its turn cleanly",
                            target_agent.label()
                        ),
                        agent: Some(target_agent),
                        conversation_id: Some(conversation.id),
                    });
                }
                if !matches!(turn.role, Role::Assistant) {
                    return Err(PrintFailure {
                        message: format!(
                            "agent {} produced an unexpected non-assistant turn",
                            target_agent.label()
                        ),
                        agent: Some(target_agent),
                        conversation_id: Some(conversation.id),
                    });
                }

                last_assistant_content = turn.content.clone();
                current_agent = conversation.active_agent;

                if opts.format == PrintFormat::Json {
                    emit_json(&PrintEvent::TurnEnd {
                        agent: target_agent.label(),
                        seq,
                        content: &last_assistant_content,
                    });
                }
            }
            TurnOutcome::Cancelled => {
                return Err(PrintFailure {
                    message: "interrupted".into(),
                    agent: Some(target_agent),
                    conversation_id: last_conversation.as_ref().map(|c| c.id),
                });
            }
            TurnOutcome::WorkerGone => {
                return Err(PrintFailure {
                    message: "worker exited before turn could complete".into(),
                    agent: Some(target_agent),
                    conversation_id: last_conversation.as_ref().map(|c| c.id),
                });
            }
        }
    }

    let conversation = last_conversation.ok_or(PrintFailure {
        message: "rotation produced no conversation snapshot".into(),
        agent: None,
        conversation_id: None,
    })?;

    Ok(PrintSuccess {
        final_content: last_assistant_content,
        conversation_id: conversation.id,
    })
}

enum TurnOutcome {
    /// Status returned to Idle after at least one non-Idle observation. The
    /// last conversation snapshot we saw is included; if a `WorkerEvent::Error`
    /// fired during the turn the message is captured here.
    Completed {
        conversation: Conversation,
        error_message: Option<String>,
    },
    /// Cancellation was requested via Ctrl+C.
    Cancelled,
    /// The worker dropped its end of the bus before the turn finished.
    WorkerGone,
}

async fn wait_for_turn(
    ev_rx: &mut tokio::sync::broadcast::Receiver<WorkerEvent>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> TurnOutcome {
    let mut latest_conversation: Option<Conversation> = None;
    let mut error_message: Option<String> = None;
    let mut left_idle = false;

    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return TurnOutcome::Cancelled;
        }
        let event = match ev_rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(n)) => {
                // We don't care about every delta; the conversation snapshot
                // re-emits after every mutation, so we'll re-converge. Log
                // and continue.
                tracing::warn!(lagged = n, "print mode lagged on event bus");
                continue;
            }
            Err(RecvError::Closed) => return TurnOutcome::WorkerGone,
        };

        match event {
            WorkerEvent::ConversationUpdated(conv) => {
                latest_conversation = Some(conv);
            }
            WorkerEvent::StatusChanged(status) => match status {
                WorkerStatus::Idle => {
                    if left_idle {
                        let conversation = match latest_conversation.take() {
                            Some(c) => c,
                            None => continue, // shouldn't happen but be defensive
                        };
                        return TurnOutcome::Completed {
                            conversation,
                            error_message,
                        };
                    }
                    // Initial idle (worker startup); ignore.
                }
                _ => {
                    left_idle = true;
                }
            },
            WorkerEvent::Error(msg) => {
                // Don't return immediately; the worker will follow with
                // StatusChanged(Idle) and we want to capture the latest
                // conversation snapshot too. Stash and let the Idle branch
                // surface it.
                error_message = Some(msg);
            }
            WorkerEvent::StatusMessage(_) => {
                // Transient status text — ignored in print mode. (Could be
                // surfaced via tracing::info! in v2 if useful.)
            }
        }
    }
}

fn emit_json(event: &PrintEvent<'_>) {
    // Print one JSON object per line on stdout. Errors writing to stdout are
    // deliberately ignored — if stdout is a closed pipe there's nothing
    // useful to do but exit, and the caller already read what we wrote.
    match serde_json::to_string(event) {
        Ok(s) => println!("{s}"),
        Err(err) => {
            tracing::warn!(%err, "failed to serialise print event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rotation_basic() {
        let r = parse_rotation("claude,codex,gpt").unwrap();
        assert_eq!(r, vec![Agent::Claude, Agent::Codex, Agent::Gpt]);
    }

    #[test]
    fn parse_rotation_trims_whitespace() {
        let r = parse_rotation("  claude , codex,gpt   ").unwrap();
        assert_eq!(r, vec![Agent::Claude, Agent::Codex, Agent::Gpt]);
    }

    #[test]
    fn parse_rotation_is_case_insensitive() {
        let r = parse_rotation("Claude,GPT,CODEX").unwrap();
        assert_eq!(r, vec![Agent::Claude, Agent::Gpt, Agent::Codex]);
    }

    #[test]
    fn parse_rotation_allows_repeats() {
        let r = parse_rotation("claude,claude").unwrap();
        assert_eq!(r, vec![Agent::Claude, Agent::Claude]);
    }

    #[test]
    fn parse_rotation_empty_errors() {
        let err = parse_rotation("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn parse_rotation_blank_entries_ignored_but_not_fully_empty() {
        // Trailing/duplicated commas are tolerated as long as something parses.
        let r = parse_rotation("claude,,codex").unwrap();
        assert_eq!(r, vec![Agent::Claude, Agent::Codex]);
        // Comma-only is still empty.
        let err = parse_rotation(",,").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn parse_rotation_unknown_agent_errors() {
        let err = parse_rotation("claude,gemini,codex").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("gemini"), "error did not mention agent: {msg}");
        assert!(
            msg.contains("claude") && msg.contains("gpt") && msg.contains("codex"),
            "error did not list valid options: {msg}"
        );
    }

    #[test]
    fn print_format_parse_accepts_text_and_json() {
        assert_eq!(PrintFormat::parse("text").unwrap(), PrintFormat::Text);
        assert_eq!(PrintFormat::parse("json").unwrap(), PrintFormat::Json);
        assert_eq!(PrintFormat::parse("JSON").unwrap(), PrintFormat::Json);
    }

    #[test]
    fn print_format_parse_rejects_unknown() {
        let err = PrintFormat::parse("yaml").unwrap_err();
        assert!(err.to_string().contains("yaml"));
    }

    #[test]
    fn print_event_turn_start_serialises_to_expected_shape() {
        let ev = PrintEvent::TurnStart {
            agent: "Claude",
            seq: 0,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "turn_start");
        assert_eq!(v["agent"], "Claude");
        assert_eq!(v["seq"], 0);
    }

    #[test]
    fn print_event_turn_end_serialises_to_expected_shape() {
        let ev = PrintEvent::TurnEnd {
            agent: "Codex",
            seq: 1,
            content: "hello world",
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "turn_end");
        assert_eq!(v["agent"], "Codex");
        assert_eq!(v["seq"], 1);
        assert_eq!(v["content"], "hello world");
    }

    #[test]
    fn print_event_error_with_known_agent() {
        let ev = PrintEvent::Error {
            agent: Some("GPT"),
            message: "auth failed",
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["agent"], "GPT");
        assert_eq!(v["message"], "auth failed");
    }

    #[test]
    fn print_event_done_includes_uuid_and_exit_code() {
        let ev = PrintEvent::Done {
            conversation_id: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
            exit_code: 0,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "done");
        assert_eq!(v["conversation_id"], "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(v["exit_code"], 0);
    }
}

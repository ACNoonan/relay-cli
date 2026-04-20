//! Single-pane chat TUI that drives the `Worker`.
//!
//! Layout:
//! ┌ agent ring ────────────────────────────────────────┐
//! │ [Claude]   GPT   Codex                   session…  │
//! ├────────────────────────────────────────────────────┤
//! │ conversation log (scrollable)                      │
//! │ …                                                  │
//! ├────────────────────────────────────────────────────┤
//! │ > input                                            │
//! │ status line                                        │
//! └────────────────────────────────────────────────────┘
//!
//! Keybindings:
//!   Enter            send typed input to the active agent
//!                    (or execute a `/command` — see `/help`)
//!   Shift+Right      rotate to next agent, auto-handoff last assistant turn
//!   Shift+Left       rotate to previous agent, auto-handoff last assistant turn
//!   Tab              rotate to next agent WITHOUT handoff (focus-only)
//!                    (consumed by slash-autocomplete popup when open)
//!   Ctrl+N           clear conversation + all provider session state
//!   Ctrl+H           toggle auto-handoff-on-rotate
//!   Ctrl+L           dump the current rendered TUI buffer to a snapshot file
//!   PgUp/PgDn/Home/End   scroll conversation log
//!   Esc              clear input (or dismiss autocomplete popup)
//!   Ctrl+C / q(empty input)   quit
//!
//!   Editor (Tier 2 #9):
//!   Left/Right       move cursor within input buffer
//!   Home/End + ←     jump to start of input (Home already scrolls log;
//!                    use Ctrl+A for start-of-input inside the buffer)
//!   Ctrl+A / Ctrl+E  cursor to start / end of input buffer
//!   Ctrl+K           kill from cursor to end-of-input → kill-ring
//!   Ctrl+U           kill from start-of-input to cursor → kill-ring
//!   Ctrl+W           kill previous word → kill-ring
//!   Alt+D            kill next word → kill-ring
//!   Ctrl+Y           yank (paste last kill at cursor)
//!   Alt+Y            yank-pop (replace previous yank with earlier kill)
//!   Ctrl+Z           undo
//!   Ctrl+R           redo (chosen over Ctrl+Shift+Z which many terminals
//!                    report as plain Ctrl+Z; Ctrl+R was previously unbound)
//!   /                when typed as the first char of an empty buffer,
//!                    opens the slash-command autocomplete popup
//!   Up/Down (popup)  navigate autocomplete entries
//!   Tab (popup)      accept highlighted command
//!   Space (popup)    accept + append space (ready for args)
//!   Esc (popup)      dismiss popup (keeps input buffer)

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    time::Duration,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::conversation::{Agent, Conversation, Role, TurnStatus};
use super::editor::{KillRing, SlashAutocomplete, UndoStack};
use super::markdown::render_markdown;
use super::persist::ConversationStore;
use super::session_picker;
use super::slash::{self, CommandRegistry, Severity, SlashOutcome, BUILTIN_COMMANDS};
use super::worker::{Worker, WorkerCommand, WorkerConfig, WorkerEvent, WorkerStatus};
use crate::tui::theme::Styles;

pub async fn run(cfg: WorkerConfig, initial_prompt: Option<String>) -> Result<()> {
    let (ev_tx, mut ev_rx) = mpsc::channel::<WorkerEvent>(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>(64);

    // Surface these before the worker takes ownership of the config.
    let harness_root = cfg.harness_root.clone();
    let styles = Styles::load(harness_root.as_deref());

    let worker = Worker::new(cfg, ev_tx)?;
    let log_path = worker.log_path();
    let worker_task = tokio::spawn(async move {
        if let Err(err) = worker.run(cmd_rx).await {
            tracing::error!(%err, "chat worker exited with error");
        }
    });

    if let Some(prompt) = initial_prompt {
        let trimmed = prompt.trim();
        if !trimmed.is_empty() {
            cmd_tx
                .send(WorkerCommand::SendToActive {
                    prompt: trimmed.to_string(),
                })
                .await
                .ok();
        }
    }

    let result = run_ui(&mut ev_rx, &cmd_tx, log_path, harness_root, &styles).await;
    // Ensure worker shuts down cleanly.
    let _ = cmd_tx.send(WorkerCommand::Quit).await;
    let _ = worker_task.await;
    result
}

/// Snapshot of the edit surface tracked by the undo/redo stack. Separate from
/// `UiState` because we only want to undo *the input buffer*, not TUI-wide
/// mutations (scroll, conversation, …).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct EditSnapshot {
    buffer: String,
    /// Cursor position as a *byte index* into `buffer`. Always on a char boundary.
    cursor: usize,
}

#[derive(Default)]
struct UiState {
    conversation: Option<Conversation>,
    status: WorkerStatus,
    status_message: String,
    input: String,
    /// Byte-offset cursor into `input`. `0..=input.len()` and always on a char
    /// boundary (all inserts go through [`insert_char_at_cursor`], which enforces
    /// this).
    input_cursor: usize,
    scroll: u16,
    /// When true we stay pinned to the bottom as new deltas arrive.
    follow_tail: bool,
    last_error: Option<String>,
    quit_requested: bool,
    /// Set to true when the user hits Ctrl+L; consumed by the main loop after the next draw.
    snapshot_requested: bool,
    /// Path of the last snapshot written — shown in the status bar so the user can find it.
    last_snapshot_path: Option<String>,
    /// Path of the worker's chat log, shown once at startup so the user knows where to look.
    log_path_banner: Option<String>,
    /// Ephemeral, UI-local messages produced by slash commands (help output,
    /// error messages, copy confirmations, …). Rendered inline at the tail
    /// of the conversation log as `system`-styled lines. Not persisted —
    /// these are transient feedback, not part of the dialogue.
    system_notes: Vec<SystemNote>,
    /// Cache of rendered markdown lines per assistant turn.
    ///
    /// Keyed on `turn_id`; the stored entry carries the content hash and
    /// width that were used to render, so a changed width OR changed
    /// content (incremental streaming update -> completion) invalidates.
    /// Using `RefCell` so the cache can be mutated from the shared-borrow
    /// render path (`terminal.draw` hands out `&UiState`).
    md_cache: RefCell<HashMap<Uuid, MarkdownCacheEntry>>,

    // ── Editor subsystems (Tier 2 #9) ───────────────────────────────────────
    /// Emacs-style kill/yank buffer. Driven by Ctrl+K/U/W, Alt+D, Ctrl+Y, Alt+Y.
    kill_ring: KillRing,
    /// Full-buffer snapshots for Ctrl+Z / Ctrl+R. Single-char inserts coalesce
    /// on a 500 ms window; hard edits (kill, yank, paste, newline) always push
    /// a distinct boundary.
    undo_stack: UndoStack<EditSnapshot>,
    /// Active slash-command popup, or `None` when the buffer doesn't look like
    /// one (see [`SlashAutocomplete::should_open`]).
    autocomplete: Option<SlashAutocomplete>,
    /// Set to `true` immediately after a yank, cleared on any other edit.
    /// Gates whether Alt+Y (yank-pop) is legal.
    last_was_yank: bool,
}

struct MarkdownCacheEntry {
    content_hash: u64,
    width: u16,
    lines: Vec<Line<'static>>,
}

/// A single inline system message produced by the slash-command layer.
/// Kept separate from [`super::conversation::Turn`] (a) to avoid polluting
/// the persisted transcript with UI ephemera, and (b) because Tier 1 #3
/// (markdown rendering) will overhaul turn rendering, and we want these
/// not to be caught up in that refactor.
struct SystemNote {
    severity: Severity,
    /// Each entry is one rendered line. Multi-line output (e.g. `/help`) pushes
    /// one note containing many lines so the block stays visually grouped.
    lines: Vec<String>,
}

async fn run_ui(
    rx: &mut mpsc::Receiver<WorkerEvent>,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
    log_path: Option<camino::Utf8PathBuf>,
    harness_root: Option<Utf8PathBuf>,
    styles: &Styles,
) -> Result<()> {
    enable_raw_mode().context("enabling raw mode for chat TUI")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating chat terminal")?;

    let mut state = UiState {
        status_message: match &log_path {
            Some(p) => format!(
                "Ready. Type to chat or /help — Shift+←/→ rotate, Ctrl+L snapshot. log: {p}"
            ),
            None => "Ready. Type to chat or /help — Shift+←/→ rotate, Ctrl+L snapshot.".to_string(),
        },
        follow_tail: true,
        log_path_banner: log_path.as_ref().map(|p| p.to_string()),
        ..UiState::default()
    };

    let registry = CommandRegistry::builtins();

    let result: Result<()> = async {
        loop {
            drain_worker_events(rx, &mut state);
            if state.quit_requested {
                break;
            }

            terminal
                .draw(|f| render(f, &state, styles))
                .context("drawing chat UI")?;

            // Honour a queued snapshot request AFTER the frame has been drawn, so the
            // snapshot captures exactly what the user was looking at.
            if state.snapshot_requested {
                state.snapshot_requested = false;
                match dump_buffer_snapshot(&mut terminal, &state) {
                    Ok(path) => {
                        state.status_message = format!("snapshot saved to {path}");
                        state.last_snapshot_path = Some(path);
                    }
                    Err(err) => {
                        state.status_message = format!("snapshot failed: {err}");
                    }
                }
            }

            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    let action = handle_key(key.code, key.modifiers, &mut state, cmd_tx, &registry);
                    match action {
                        KeyAction::Continue => {}
                        KeyAction::Quit => break,
                        KeyAction::OpenResumePicker => {
                            handle_resume(
                                &mut terminal,
                                &mut state,
                                cmd_tx,
                                harness_root.as_deref(),
                            )
                            .await;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    result
}

/// What `handle_key` wants the caller (the event loop) to do next. Breaking
/// this out keeps the keymap itself synchronous and small; async work (like
/// opening the session picker) happens in the loop body, not in the match.
enum KeyAction {
    Continue,
    Quit,
    OpenResumePicker,
}

/// Suspend the chat TUI's raw-mode + alt-screen for the duration of the
/// picker, invoke `session_picker::pick_session`, and — if the user selected
/// a conversation — load it from disk and send a `LoadConversation` command
/// to the worker. The picker manages its own terminal state, so we just
/// step out of the way cleanly and step back in afterwards.
async fn handle_resume(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut UiState,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
    harness_root: Option<&camino::Utf8Path>,
) {
    let Some(root) = harness_root else {
        push_note(
            state,
            Severity::Error,
            vec!["/resume: no harness initialised (run `relay init` first).".to_string()],
        );
        return;
    };

    // Suspend our terminal state. The picker will enter its own alt-screen.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();

    let picked = session_picker::pick_session(root).await;

    // Whatever happened, put our terminal back together before continuing.
    let _ = enable_raw_mode();
    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
    let _ = terminal.clear();

    match picked {
        Err(err) => push_note(
            state,
            Severity::Error,
            vec![format!("/resume failed: {err}")],
        ),
        Ok(None) => push_note(
            state,
            Severity::Info,
            vec!["/resume: cancelled.".to_string()],
        ),
        Ok(Some(id)) => {
            let store = ConversationStore::open(Some(root.to_path_buf()));
            if !store.is_enabled() {
                push_note(
                    state,
                    Severity::Error,
                    vec!["/resume: harness not initialised.".to_string()],
                );
                return;
            }
            match store.load(id) {
                Ok(conv) => {
                    let _ = cmd_tx
                        .send(WorkerCommand::LoadConversation(Box::new(conv)))
                        .await;
                    push_note(
                        state,
                        Severity::Success,
                        vec![format!("/resume: loaded {}.", short_uuid(&id.to_string()))],
                    );
                }
                Err(err) => push_note(
                    state,
                    Severity::Error,
                    vec![format!("/resume: load failed — {err}")],
                ),
            }
        }
    }
}

fn push_note(state: &mut UiState, severity: Severity, lines: Vec<String>) {
    state.follow_tail = true;
    state.system_notes.push(SystemNote { severity, lines });
}

fn short_uuid(s: &str) -> String {
    s.chars().take(8).collect()
}

fn dump_buffer_snapshot(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &UiState,
) -> Result<String> {
    let buf = terminal.current_buffer_mut().clone();
    let area = buf.area();
    let mut out = String::new();
    out.push_str(&format!(
        "# relay chat TUI snapshot  {}x{}  ts:{}\n",
        area.width,
        area.height,
        chrono::Utc::now().to_rfc3339()
    ));
    if let Some(p) = &state.log_path_banner {
        out.push_str(&format!("# chat log: {p}\n"));
    }
    out.push_str("# ────────────────────────────────────────────────────────────\n");
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(area.x + x, area.y + y)];
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }

    // Choose snapshot destination: prefer the harness logs dir if we have a log_path_banner,
    // otherwise fall back to the temp dir so the snapshot always lands somewhere readable.
    let dest = match &state.log_path_banner {
        Some(p) => {
            let parent = std::path::Path::new(p)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::env::temp_dir());
            parent.join(format!(
                "relay-chat-snapshot-{}.txt",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            ))
        }
        None => std::env::temp_dir().join(format!(
            "relay-chat-snapshot-{}.txt",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        )),
    };
    std::fs::write(&dest, out).with_context(|| format!("writing snapshot to {dest:?}"))?;
    Ok(dest.to_string_lossy().into_owned())
}

fn drain_worker_events(rx: &mut mpsc::Receiver<WorkerEvent>, state: &mut UiState) {
    while let Ok(event) = rx.try_recv() {
        match event {
            WorkerEvent::ConversationUpdated(c) => {
                state.conversation = Some(c);
            }
            WorkerEvent::StatusChanged(s) => {
                state.status = s;
            }
            WorkerEvent::StatusMessage(m) => {
                state.status_message = m;
            }
            WorkerEvent::Error(e) => {
                state.last_error = Some(e);
            }
        }
    }
}

/// Handle a single key event. Returns a [`KeyAction`] telling the event loop
/// whether to continue, exit, or perform an async side-effect (currently
/// only opening the session picker for `/resume`).
///
/// Dispatch order:
/// 1. Non-editor chrome chords (Ctrl+C, Ctrl+N/H/L, Shift+Left/Right, paging).
/// 2. If the autocomplete popup is open, route navigation keys (Up/Down/Tab/
///    Esc/Space) to it. Printable keys continue to the editor path so they
///    filter the popup.
/// 3. Editor keys: cursor movement (Left/Right/Ctrl+A/Ctrl+E), kill-ring
///    (Ctrl+K/U/W, Alt+D, Ctrl+Y, Alt+Y), undo (Ctrl+Z), redo (Ctrl+R),
///    backspace/delete, Enter, and printable-char insertion.
fn handle_key(
    code: KeyCode,
    mods: KeyModifiers,
    state: &mut UiState,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
    registry: &CommandRegistry,
) -> KeyAction {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let shift = mods.contains(KeyModifiers::SHIFT);
    let alt = mods.contains(KeyModifiers::ALT);

    // ── 1. Non-editor chrome chords ─────────────────────────────────────────
    match (code, ctrl, shift) {
        (KeyCode::Char('c'), true, _) => {
            state.quit_requested = true;
            return KeyAction::Quit;
        }
        (KeyCode::Char('q'), false, false) if state.input.is_empty() => {
            state.quit_requested = true;
            return KeyAction::Quit;
        }
        (KeyCode::Char('n'), true, _) => {
            let _ = cmd_tx.try_send(WorkerCommand::NewConversation);
            return KeyAction::Continue;
        }
        (KeyCode::Char('h'), true, _) => {
            let _ = cmd_tx.try_send(WorkerCommand::ToggleAutoHandoff);
            return KeyAction::Continue;
        }
        (KeyCode::Char('l'), true, _) => {
            state.snapshot_requested = true;
            return KeyAction::Continue;
        }
        (KeyCode::Right, _, true) => {
            if let Some(c) = &state.conversation {
                let to = c.active_agent.next();
                let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                    agent: to,
                    handoff_last_assistant: c.auto_handoff_enabled,
                });
            }
            return KeyAction::Continue;
        }
        (KeyCode::Left, _, true) => {
            if let Some(c) = &state.conversation {
                let to = c.active_agent.prev();
                let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                    agent: to,
                    handoff_last_assistant: c.auto_handoff_enabled,
                });
            }
            return KeyAction::Continue;
        }
        (KeyCode::PageUp, _, _) => {
            state.follow_tail = false;
            state.scroll = state.scroll.saturating_sub(10);
            return KeyAction::Continue;
        }
        (KeyCode::PageDown, _, _) => {
            state.scroll = state.scroll.saturating_add(10);
            return KeyAction::Continue;
        }
        (KeyCode::Home, _, _) => {
            // Log scroll to top. Inside-buffer start-of-line is Ctrl+A.
            state.follow_tail = false;
            state.scroll = 0;
            return KeyAction::Continue;
        }
        (KeyCode::End, _, _) => {
            state.follow_tail = true;
            return KeyAction::Continue;
        }
        _ => {}
    }

    // ── 2. Autocomplete popup intercepts ────────────────────────────────────
    // Tab, Up/Down, Esc, and Space ONLY when the popup is open. Otherwise Tab
    // falls through to agent-rotation below (preserves existing behavior).
    if state.autocomplete.is_some() {
        match code {
            KeyCode::Tab => {
                autocomplete_accept(state, /* append_space= */ false);
                return KeyAction::Continue;
            }
            KeyCode::Char(' ') => {
                autocomplete_accept(state, /* append_space= */ true);
                return KeyAction::Continue;
            }
            KeyCode::Up => {
                if let Some(ac) = state.autocomplete.as_mut() {
                    ac.prev();
                }
                return KeyAction::Continue;
            }
            KeyCode::Down => {
                if let Some(ac) = state.autocomplete.as_mut() {
                    ac.next();
                }
                return KeyAction::Continue;
            }
            KeyCode::Esc => {
                state.autocomplete = None;
                return KeyAction::Continue;
            }
            _ => {
                // Fall through — printable keys, backspace, etc. still mutate
                // the buffer and re-drive the popup via refresh_autocomplete.
            }
        }
    }

    // Tab with no popup = rotate agent (legacy behavior).
    if matches!(code, KeyCode::Tab) {
        if let Some(c) = &state.conversation {
            let to = c.active_agent.next();
            let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                agent: to,
                handoff_last_assistant: false,
            });
        }
        return KeyAction::Continue;
    }

    // ── 3. Editor keys ──────────────────────────────────────────────────────
    match (code, ctrl, alt) {
        // Cursor movement
        (KeyCode::Left, false, false) => {
            move_cursor_left(state);
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
        }
        (KeyCode::Right, false, false) => {
            move_cursor_right(state);
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
        }
        (KeyCode::Char('a'), true, false) => {
            state.input_cursor = 0;
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
        }
        (KeyCode::Char('e'), true, false) => {
            state.input_cursor = state.input.len();
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
        }

        // Kill commands
        (KeyCode::Char('k'), true, false) => {
            editor_push_undo(state);
            let tail = state.input.split_off(state.input_cursor);
            state.kill_ring.kill(&tail, /* prepend= */ false);
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }
        (KeyCode::Char('u'), true, false) => {
            editor_push_undo(state);
            let head: String = state.input.drain(..state.input_cursor).collect();
            state.kill_ring.kill(&head, /* prepend= */ true);
            state.input_cursor = 0;
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }
        (KeyCode::Char('w'), true, false) => {
            editor_push_undo(state);
            kill_previous_word(state);
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }
        (KeyCode::Char('d'), false, true) => {
            editor_push_undo(state);
            kill_next_word(state);
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }

        // Yank / yank-pop
        (KeyCode::Char('y'), true, false) => {
            editor_push_undo(state);
            if let Some(text) = state.kill_ring.yank() {
                let text = text.to_string();
                insert_str_at_cursor(state, &text);
            }
            state.kill_ring.mark_boundary();
            state.last_was_yank = true;
            refresh_autocomplete(state);
        }
        (KeyCode::Char('y'), false, true) => {
            // Alt+Y: yank-pop. Only legal immediately after a yank.
            if state.last_was_yank {
                // Remove the previously-yanked text, rotate, then insert the
                // new head entry at the same cursor position.
                if let Some(prev) = state.kill_ring.yank() {
                    let prev_len = prev.len();
                    let start = state.input_cursor.saturating_sub(prev_len);
                    state.input.drain(start..state.input_cursor);
                    state.input_cursor = start;
                    let next_text = state.kill_ring.yank_pop().map(str::to_string);
                    if let Some(t) = next_text {
                        insert_str_at_cursor(state, &t);
                    }
                }
                state.last_was_yank = true;
                refresh_autocomplete(state);
            }
        }

        // Undo / redo
        (KeyCode::Char('z'), true, false) => {
            let current = EditSnapshot {
                buffer: state.input.clone(),
                cursor: state.input_cursor,
            };
            if let Some(prev) = state.undo_stack.undo(current) {
                state.input = prev.buffer;
                state.input_cursor = prev.cursor.min(state.input.len());
            }
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }
        (KeyCode::Char('r'), true, false) => {
            // Redo. Ctrl+R chosen over Ctrl+Shift+Z because many terminals
            // report the shifted variant as plain Ctrl+Z, and Ctrl+Y is
            // already bound to yank.
            let current = EditSnapshot {
                buffer: state.input.clone(),
                cursor: state.input_cursor,
            };
            if let Some(next) = state.undo_stack.redo(current) {
                state.input = next.buffer;
                state.input_cursor = next.cursor.min(state.input.len());
            }
            state.kill_ring.mark_boundary();
            state.last_was_yank = false;
            refresh_autocomplete(state);
        }

        // Enter / Backspace / Delete / Esc
        (KeyCode::Enter, false, false) => {
            let text = std::mem::take(&mut state.input);
            state.input_cursor = 0;
            state.autocomplete = None;
            state.last_was_yank = false;
            state.kill_ring.mark_boundary();
            // A send-or-command is a hard boundary for the undo stack; the
            // next keystroke should start fresh.
            state.undo_stack = UndoStack::new();

            let trimmed = text.trim();
            if trimmed.is_empty() {
                return KeyAction::Continue;
            }
            if let Some(parsed) = slash::parse(trimmed) {
                let outcome = slash::resolve(&parsed, registry);
                return apply_outcome(outcome, state, cmd_tx);
            }
            state.follow_tail = true;
            let _ = cmd_tx.try_send(WorkerCommand::SendToActive {
                prompt: trimmed.to_string(),
            });
        }
        (KeyCode::Esc, false, false) => {
            state.input.clear();
            state.input_cursor = 0;
            state.autocomplete = None;
            state.last_was_yank = false;
            state.kill_ring.mark_boundary();
        }
        (KeyCode::Backspace, false, false) => {
            delete_left_of_cursor(state);
            state.last_was_yank = false;
            state.kill_ring.mark_boundary();
            refresh_autocomplete(state);
        }
        (KeyCode::Delete, false, false) => {
            delete_right_of_cursor(state);
            state.last_was_yank = false;
            state.kill_ring.mark_boundary();
            refresh_autocomplete(state);
        }

        // Printable char insertion (no modifier, or Shift only).
        (KeyCode::Char(ch), false, false) => {
            insert_char_edit(state, ch);
        }
        _ => {}
    }
    KeyAction::Continue
}

/// Move cursor one char to the left. No-op if already at 0.
fn move_cursor_left(state: &mut UiState) {
    if state.input_cursor == 0 {
        return;
    }
    // Walk back one grapheme — we use char boundaries as a pragmatic
    // approximation; the buffer is plain prompts, not combining-char text.
    let mut new = state.input_cursor - 1;
    while new > 0 && !state.input.is_char_boundary(new) {
        new -= 1;
    }
    state.input_cursor = new;
}

fn move_cursor_right(state: &mut UiState) {
    if state.input_cursor >= state.input.len() {
        return;
    }
    let mut new = state.input_cursor + 1;
    while new < state.input.len() && !state.input.is_char_boundary(new) {
        new += 1;
    }
    state.input_cursor = new;
}

/// Insert a char at the cursor and advance. Drives undo (coalescing) and the
/// slash-command popup on every edit.
fn insert_char_edit(state: &mut UiState, ch: char) {
    // Hard-boundary pushes: space and newline. Everything else coalesces.
    let hard_boundary = ch == ' ' || ch == '\n';
    let snapshot = EditSnapshot {
        buffer: state.input.clone(),
        cursor: state.input_cursor,
    };
    if hard_boundary {
        state.undo_stack.push(snapshot);
    } else {
        state.undo_stack.push_edit(snapshot);
    }
    state.kill_ring.mark_boundary();
    state.last_was_yank = false;
    insert_char_at_cursor(state, ch);
    refresh_autocomplete(state);
}

fn insert_char_at_cursor(state: &mut UiState, ch: char) {
    // Safe because input_cursor is always a char boundary (invariant we
    // maintain across all edits in this module).
    state.input.insert(state.input_cursor, ch);
    state.input_cursor += ch.len_utf8();
}

fn insert_str_at_cursor(state: &mut UiState, s: &str) {
    state.input.insert_str(state.input_cursor, s);
    state.input_cursor += s.len();
}

fn delete_left_of_cursor(state: &mut UiState) {
    if state.input_cursor == 0 {
        return;
    }
    let mut new = state.input_cursor - 1;
    while new > 0 && !state.input.is_char_boundary(new) {
        new -= 1;
    }
    editor_push_undo(state);
    state.input.drain(new..state.input_cursor);
    state.input_cursor = new;
}

fn delete_right_of_cursor(state: &mut UiState) {
    if state.input_cursor >= state.input.len() {
        return;
    }
    let mut new = state.input_cursor + 1;
    while new < state.input.len() && !state.input.is_char_boundary(new) {
        new += 1;
    }
    editor_push_undo(state);
    state.input.drain(state.input_cursor..new);
}

/// Push the current buffer as a *hard* undo boundary. Used by kill/yank and
/// backspace/delete — anything that is not a typing burst.
fn editor_push_undo(state: &mut UiState) {
    state.undo_stack.push(EditSnapshot {
        buffer: state.input.clone(),
        cursor: state.input_cursor,
    });
}

/// Kill the word immediately before the cursor, Emacs-style. A "word" is one
/// or more ASCII alphanumerics; preceding whitespace is killed with it.
fn kill_previous_word(state: &mut UiState) {
    let bytes = state.input.as_bytes();
    let mut end = state.input_cursor;
    // Walk backward over non-word chars first (typical Emacs Ctrl+W:
    // kill-whitespace-and-word).
    while end > 0 && !is_word_byte(bytes[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == state.input_cursor {
        // nothing to kill
        return;
    }
    let killed: String = state.input.drain(start..state.input_cursor).collect();
    state.input_cursor = start;
    state.kill_ring.kill(&killed, /* prepend= */ true);
}

fn kill_next_word(state: &mut UiState) {
    let bytes = state.input.as_bytes();
    let len = state.input.len();
    let mut start = state.input_cursor;
    while start < len && !is_word_byte(bytes[start]) {
        start += 1;
    }
    let mut end = start;
    while end < len && is_word_byte(bytes[end]) {
        end += 1;
    }
    if end == state.input_cursor {
        return;
    }
    let killed: String = state.input.drain(state.input_cursor..end).collect();
    state.kill_ring.kill(&killed, /* prepend= */ false);
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Re-evaluate the autocomplete popup against the current input buffer.
/// Opens, refreshes, or closes as appropriate — callers don't need to know
/// the transition rules.
fn refresh_autocomplete(state: &mut UiState) {
    if SlashAutocomplete::should_open(&state.input) {
        // Note: we deliberately keep the popup open even when the buffer
        // exactly matches a complete command (e.g. `/help`). The user can
        // still want to swap to a different command, and `should_open` will
        // close the popup naturally as soon as they type a space (start of
        // arguments) — at which point they're past the name portion.
        match state.autocomplete.as_mut() {
            Some(ac) => {
                if !ac.refresh(&state.input) {
                    state.autocomplete = None;
                }
            }
            None => {
                state.autocomplete = SlashAutocomplete::new(&state.input);
            }
        }
    } else {
        state.autocomplete = None;
    }
}

/// Replace the input buffer with the selected autocomplete entry. When
/// `append_space` is true, adds a trailing space so the user can immediately
/// type arguments (Space accepts in this mode).
fn autocomplete_accept(state: &mut UiState, append_space: bool) {
    let Some(ac) = state.autocomplete.as_ref() else {
        return;
    };
    let Some(mut replacement) = ac.accept() else {
        state.autocomplete = None;
        return;
    };
    if append_space {
        replacement.push(' ');
    }
    // Treat acceptance as a hard undo boundary.
    editor_push_undo(state);
    state.input = replacement;
    state.input_cursor = state.input.len();
    state.kill_ring.mark_boundary();
    state.last_was_yank = false;
    // If we appended a space the popup should close (space is the "past the
    // name" signal). If we didn't, keep it open so the user sees confirmation
    // — but the next keystroke will refresh naturally.
    if append_space {
        state.autocomplete = None;
    } else {
        refresh_autocomplete(state);
    }
}

/// Apply a [`SlashOutcome`] against `UiState` / the worker command channel.
///
/// This is the only code path that can ask the event loop to open the
/// session picker (the outcome [`SlashOutcome::RequireSessionPick`] maps to
/// [`KeyAction::OpenResumePicker`]). Everything else is local or sent via
/// the worker channel.
fn apply_outcome(
    outcome: SlashOutcome,
    state: &mut UiState,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
) -> KeyAction {
    match outcome {
        SlashOutcome::Consumed => KeyAction::Continue,
        SlashOutcome::ShowMessage(msg, sev) => {
            push_note(state, sev, vec![msg]);
            KeyAction::Continue
        }
        SlashOutcome::ShowHelp => {
            push_note(state, Severity::Info, help_lines());
            KeyAction::Continue
        }
        SlashOutcome::ShowHotkeys => {
            push_note(state, Severity::Info, hotkey_lines());
            KeyAction::Continue
        }
        SlashOutcome::ClearConversation => {
            let _ = cmd_tx.try_send(WorkerCommand::NewConversation);
            let short = state
                .conversation
                .as_ref()
                .map(|c| short_uuid(&c.id.to_string()))
                .unwrap_or_else(|| "new".to_string());
            push_note(
                state,
                Severity::Success,
                vec![format!("/new: started new conversation {short}.")],
            );
            KeyAction::Continue
        }
        SlashOutcome::RequireSessionPick => KeyAction::OpenResumePicker,
        SlashOutcome::Compact => {
            let _ = cmd_tx.try_send(WorkerCommand::CompactNow);
            push_note(
                state,
                Severity::Info,
                vec!["/compact: requested — watch the status bar for results.".to_string()],
            );
            KeyAction::Continue
        }
        SlashOutcome::Copy => {
            match copy_last_assistant(state) {
                Ok(len) => push_note(
                    state,
                    Severity::Success,
                    vec![format!(
                        "/copy: {len} chars of the last assistant message copied to clipboard."
                    )],
                ),
                Err(msg) => push_note(state, Severity::Error, vec![format!("/copy: {msg}")]),
            }
            KeyAction::Continue
        }
        SlashOutcome::Handoff(agent) => {
            let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                agent,
                handoff_last_assistant: true,
            });
            push_note(
                state,
                Severity::Info,
                vec![format!("/handoff → {}.", agent.label())],
            );
            KeyAction::Continue
        }
        SlashOutcome::Focus(agent) => {
            let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                agent,
                handoff_last_assistant: false,
            });
            push_note(
                state,
                Severity::Info,
                vec![format!("/focus → {}.", agent.label())],
            );
            KeyAction::Continue
        }
        SlashOutcome::Quit => {
            state.quit_requested = true;
            KeyAction::Quit
        }
    }
}

/// Build the inline `/help` body from the registry.
fn help_lines() -> Vec<String> {
    let mut out = Vec::with_capacity(BUILTIN_COMMANDS.len() + 2);
    out.push("Slash commands:".to_string());
    for entry in BUILTIN_COMMANDS {
        let suffix = if entry.args_hint.is_empty() {
            String::new()
        } else {
            format!(" {}", entry.args_hint)
        };
        out.push(format!(
            "  /{name}{suffix}  —  {desc}",
            name = entry.name,
            suffix = suffix,
            desc = entry.description,
        ));
    }
    out.push("Tip: `/?` is a shortcut for `/help`.".to_string());
    out
}

/// Build the inline `/hotkeys` body. Source of truth is this list — mirrors
/// the module-top-of-file comment.
fn hotkey_lines() -> Vec<String> {
    vec![
        "Hotkeys:".to_string(),
        "  Enter            send input (or run /command)".to_string(),
        "  Shift+Right/Left rotate agent + auto-handoff last turn".to_string(),
        "  Tab              rotate agent focus-only (no handoff)".to_string(),
        "                   (consumed by /-autocomplete popup when open)".to_string(),
        "  Ctrl+N           clear conversation + session state".to_string(),
        "  Ctrl+H           toggle auto-handoff-on-rotate".to_string(),
        "  Ctrl+L           snapshot the current TUI buffer to a file".to_string(),
        "  PgUp/PgDn        scroll conversation log".to_string(),
        "  Home / End       jump to top / follow tail".to_string(),
        "  Esc              clear input (or dismiss popup)".to_string(),
        "  Ctrl+C           quit".to_string(),
        "".to_string(),
        "Editor:".to_string(),
        "  Left/Right       move cursor in input buffer".to_string(),
        "  Ctrl+A / Ctrl+E  cursor to start / end of input".to_string(),
        "  Ctrl+K           kill from cursor → end of input (kill-ring)".to_string(),
        "  Ctrl+U           kill from start of input → cursor (kill-ring)".to_string(),
        "  Ctrl+W           kill previous word (kill-ring)".to_string(),
        "  Alt+D            kill next word (kill-ring)".to_string(),
        "  Ctrl+Y           yank (paste last kill)".to_string(),
        "  Alt+Y            yank-pop (cycle older kills, after a yank)".to_string(),
        "  Ctrl+Z           undo (single-char edits coalesce in 500 ms bursts)".to_string(),
        "  Ctrl+R           redo".to_string(),
        "  /                opens autocomplete when first char of input".to_string(),
        "  Up/Down (popup)  navigate completions".to_string(),
        "  Tab (popup)      accept selected command".to_string(),
        "  Space (popup)    accept + ready for arguments".to_string(),
    ]
}

/// Copy the most recent assistant turn to the system clipboard. Returns the
/// character count on success, or a user-facing error message on failure.
fn copy_last_assistant(state: &UiState) -> Result<usize, String> {
    let Some(conv) = &state.conversation else {
        return Err("no conversation yet.".to_string());
    };
    let Some(turn) = conv.last_assistant_turn() else {
        return Err("no assistant message to copy yet.".to_string());
    };
    let content = turn.content.clone();
    let count = content.chars().count();
    // arboard's Clipboard constructor can fail on headless systems (no
    // display / no pasteboard); surface a friendly note rather than
    // crashing.
    let mut clip = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    clip.set_text(content)
        .map_err(|e| format!("clipboard write failed: {e}"))?;
    Ok(count)
}

fn render(f: &mut Frame<'_>, state: &UiState, styles: &Styles) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // agent ring chrome
            Constraint::Min(3),    // conversation log
            Constraint::Length(1), // visual breathing room — keeps the last message from
            //                        butting against the input box's top border
            Constraint::Length(3), // input box
            Constraint::Length(1), // status line
        ])
        .split(f.area());

    render_agent_ring(f, chunks[0], state);
    render_conversation(f, chunks[1], state, styles);
    // chunks[2] deliberately left blank as a spacer row.
    render_input(f, chunks[3], state);
    render_status(f, chunks[4], state);

    // Autocomplete popup is drawn LAST so it overlays the input + spacer rows.
    // Anchored relative to the input box; falls back to drawing below the
    // conversation log when the input box is the bottom of the screen and we
    // would clip otherwise.
    if state.autocomplete.is_some() {
        render_autocomplete_popup(f, chunks[3], chunks[1], state, styles);
    }
}

fn render_agent_ring(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let active = state
        .conversation
        .as_ref()
        .map(|c| c.active_agent)
        .unwrap_or_default();
    let auto_handoff = state
        .conversation
        .as_ref()
        .map(|c| c.auto_handoff_enabled)
        .unwrap_or(true);

    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, agent) in Agent::RING.iter().enumerate() {
        let active_here = *agent == active;
        let color = agent_color(*agent);
        let style = if active_here {
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color).add_modifier(Modifier::DIM)
        };
        let label = if active_here {
            format!(" ▶ {} ", agent.label())
        } else {
            format!("   {}   ", agent.label())
        };
        spans.push(Span::styled(label, style));
        if i < Agent::RING.len() - 1 {
            spans.push(Span::raw("  "));
        }
    }

    let session_hint = state
        .conversation
        .as_ref()
        .and_then(|c| c.sessions.session_id_for(active))
        .map(|id| format!("  session:{}", &id[..id.len().min(8)]))
        .unwrap_or_default();
    spans.push(Span::styled(
        format!(
            "   auto-handoff:{}",
            if auto_handoff { "on" } else { "off" }
        ),
        Style::default().add_modifier(Modifier::DIM),
    ));
    if !session_hint.is_empty() {
        spans.push(Span::styled(
            session_hint,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }

    let block = Block::default().borders(Borders::ALL).title(" relay chat ");
    let para = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(para, area);
}

fn render_conversation(f: &mut Frame<'_>, area: Rect, state: &UiState, styles: &Styles) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    // Width available inside the bordered block, less the 2-cell content
    // indent we apply to every turn body line.
    let inner_width = area.width.saturating_sub(2).max(1);
    let md_width = inner_width.saturating_sub(2).max(10);
    if let Some(c) = &state.conversation {
        if c.turns.is_empty() {
            lines.push(Line::styled(
                "No turns yet. Type a prompt and press Enter (or /help).",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        for turn in &c.turns {
            let prefix = match turn.role {
                Role::User => "you".to_string(),
                Role::Handoff => format!("↪ handoff → {}", turn.agent.label().to_lowercase()),
                Role::Assistant => turn.agent.label().to_lowercase(),
                Role::System => "system".to_string(),
            };
            let prefix_style = match turn.role {
                Role::User => Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                Role::Handoff => Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                Role::Assistant => Style::default()
                    .fg(agent_color(turn.agent))
                    .add_modifier(Modifier::BOLD),
                Role::System => Style::default().fg(Color::DarkGray),
            };
            let suffix = match turn.status {
                TurnStatus::Streaming => " …",
                TurnStatus::Error => " ⚠",
                TurnStatus::Complete => "",
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{prefix}{suffix}"), prefix_style),
                Span::raw(""),
            ]));

            // Only completed assistant turns get markdown rendering. During
            // streaming we deliberately fall back to plain text — re-parsing
            // per delta is expensive and flickers inline code/fence styles
            // on and off as a block becomes complete. See `markdown.rs` for
            // rationale.
            let use_markdown = matches!(turn.role, Role::Assistant)
                && matches!(turn.status, TurnStatus::Complete)
                && !turn.content.is_empty();

            if use_markdown {
                let cached = cached_markdown(state, turn.id, &turn.content, md_width, styles);
                for line in cached {
                    // Prepend the 2-cell body indent so the styled lines
                    // align with the plain-text path below.
                    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
                    spans.push(Span::raw("  "));
                    spans.extend(line.spans);
                    lines.push(Line::from(spans));
                }
            } else {
                for line in turn.content.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::raw(line.to_string()),
                    ]));
                }
            }
            if turn.content.is_empty() && turn.status == TurnStatus::Streaming {
                lines.push(Line::from(vec![Span::styled(
                    "  (waiting for first token…)",
                    Style::default().add_modifier(Modifier::DIM),
                )]));
            }
            lines.push(Line::raw(""));
        }
    }

    // Inline slash-command output — appended at the tail so it's always
    // visible alongside the latest agent turn. Not persisted (UI-local).
    for note in &state.system_notes {
        let body_style = match note.severity {
            Severity::Info => styles.dim(),
            Severity::Success => styles.success(),
            Severity::Warning => styles.warning(),
            Severity::Error => styles.danger(),
        };
        let tag = match note.severity {
            Severity::Info => "system",
            Severity::Success => "system ✓",
            Severity::Warning => "system ⚠",
            Severity::Error => "system ✗",
        };
        lines.push(Line::from(vec![Span::styled(
            tag.to_string(),
            styles.label(),
        )]));
        for l in &note.lines {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(l.clone(), body_style),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" conversation ");
    let inner_width = inner_width as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    // Paragraph.scroll counts *wrapped* rows, not logical lines. Computing scroll
    // against `lines.len()` undercounts when content wraps, which pushes the tail
    // off-screen. Estimate wrapped rows by ceil(line_width / inner_width).
    let wrapped_total: usize = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width)
            }
        })
        .sum();
    let scroll = if state.follow_tail {
        wrapped_total.saturating_sub(inner_height) as u16
    } else {
        state.scroll
    };
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn render_input(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let active = state
        .conversation
        .as_ref()
        .map(|c| c.active_agent)
        .unwrap_or_default();
    let title = format!(" → {} ", active.label());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(agent_color(active)))
        .title(title);

    // Render the buffer with a visible cursor block. We split on the byte
    // cursor (which we maintain on a char boundary) and render the char under
    // the cursor with a reverse-video style. When the cursor is past the end,
    // emit a trailing space so there's something to highlight.
    let line: Line<'_> = if state.input.is_empty() {
        Line::from(vec![
            Span::raw("› "),
            Span::styled(
                "▏",
                Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                "type a message, Enter to send",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ])
    } else {
        let cursor = state.input_cursor.min(state.input.len());
        let (before, after) = state.input.split_at(cursor);
        let mut spans: Vec<Span<'_>> = vec![Span::raw("› "), Span::raw(before)];
        if after.is_empty() {
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        } else {
            // Highlight just the first char under the cursor; render the rest
            // normally.
            let mut chars = after.char_indices();
            let (_, first_ch) = chars.next().expect("after non-empty");
            let after_first_byte = chars.next().map(|(i, _)| i).unwrap_or(after.len());
            spans.push(Span::styled(
                first_ch.to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::raw(after[after_first_byte..].to_string()));
        }
        Line::from(spans)
    };

    let para = Paragraph::new(line).block(block);
    f.render_widget(para, area);
}

/// Floating popup listing slash-command completions. Anchored just *above* the
/// input box when there's vertical room in the conversation log area;
/// otherwise it overlays the bottom of the log itself.
fn render_autocomplete_popup(
    f: &mut Frame<'_>,
    input_area: Rect,
    log_area: Rect,
    state: &UiState,
    styles: &Styles,
) {
    let Some(ac) = state.autocomplete.as_ref() else {
        return;
    };
    let items = ac.items();
    if items.is_empty() {
        return;
    }

    // One row per item plus 2 rows of border.
    let rows = items.len() as u16;
    let height = (rows + 2).min(input_area.y.saturating_sub(log_area.y).max(3));

    // Width: longest "name + 2 + args_hint + 2 + description", capped at 60.
    let max_name = items.iter().map(|i| i.name.len()).max().unwrap_or(4);
    let inner_width: u16 = items
        .iter()
        .map(|i| {
            (max_name
                + 2
                + i.args_hint.len()
                + if i.args_hint.is_empty() { 0 } else { 2 }
                + i.description.len()) as u16
        })
        .max()
        .unwrap_or(40);
    let width = (inner_width + 4).clamp(20, 60);

    // Anchor: prefer just above the input box, left-aligned to the input box.
    // If we'd clip the top of the log, fall back to overlaying the bottom of
    // the log.
    let popup_y = if input_area.y >= log_area.y + height {
        input_area.y.saturating_sub(height)
    } else {
        log_area.y + log_area.height.saturating_sub(height)
    };
    let popup_x = input_area.x;
    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: width.min(input_area.width),
        height,
    };

    // Build the lines.
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let selected = idx == ac.selected_index();
        let name_style = if selected {
            styles.selected()
        } else {
            styles.accent()
        };
        let desc_style = if selected {
            styles.selected()
        } else {
            styles.dim()
        };
        let mut spans: Vec<Span<'_>> = Vec::with_capacity(5);
        // Pad the name column so descriptions line up.
        let padded_name = format!(" /{}{} ", item.name, " ".repeat(max_name - item.name.len()));
        spans.push(Span::styled(padded_name, name_style));
        if !item.args_hint.is_empty() {
            spans.push(Span::styled(format!("{} ", item.args_hint), desc_style));
        }
        spans.push(Span::styled(item.description.to_string(), desc_style));
        lines.push(Line::from(spans));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border_active())
        .title(" / autocomplete ");
    let para = Paragraph::new(lines).block(block);
    // Clear the underlying cells first so the popup isn't transparent over
    // any conversation text already rendered there.
    f.render_widget(ratatui::widgets::Clear, popup_area);
    f.render_widget(para, popup_area);
}

fn render_status(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let status_text = match &state.status {
        WorkerStatus::Idle => "idle".to_string(),
        WorkerStatus::Submitting { agent } => format!("submitting to {}…", agent.label()),
        WorkerStatus::Streaming { agent } => format!("streaming {}…", agent.label()),
        WorkerStatus::QueuedHandoff { to } => format!("handoff → {} queued", to.label()),
        WorkerStatus::Error { message } => format!("error: {}", truncate(message, 80)),
    };

    let err_suffix = state
        .last_error
        .as_ref()
        .map(|e| format!("  |  last error: {}", truncate(e, 60)))
        .unwrap_or_default();

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", status_text),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" "),
        Span::styled(
            state.status_message.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(err_suffix, Style::default().fg(Color::Red)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn agent_color(agent: Agent) -> Color {
    match agent {
        Agent::Claude => Color::Rgb(255, 140, 66),
        Agent::Gpt => Color::Rgb(16, 163, 127),
        Agent::Codex => Color::Rgb(120, 132, 255),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Fetch a cached markdown render of `content` for `turn_id` at `width`, or
/// produce and cache one on miss. Invalidates on any change to `width` OR
/// `content` (keyed by a default hasher over the content bytes).
fn cached_markdown(
    state: &UiState,
    turn_id: Uuid,
    content: &str,
    width: u16,
    styles: &Styles,
) -> Vec<Line<'static>> {
    let hash = hash_content(content);
    let mut cache = state.md_cache.borrow_mut();
    if let Some(entry) = cache.get(&turn_id) {
        if entry.content_hash == hash && entry.width == width {
            return entry.lines.clone();
        }
    }
    let lines = render_markdown(content, width, styles);
    cache.insert(
        turn_id,
        MarkdownCacheEntry {
            content_hash: hash,
            width,
            lines: lines.clone(),
        },
    );
    lines
}

fn hash_content(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

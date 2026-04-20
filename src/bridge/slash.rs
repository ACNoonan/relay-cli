//! Slash-command parsing and built-in command registry for `relay chat`.
//!
//! This module is the pure-data half of the slash-command system (Tier 1 #1).
//! Parsing a line of input produces a [`ParsedCommand`]; looking it up against
//! [`CommandRegistry::builtins`] either yields a [`SlashCommand`] that the TUI
//! executes (by producing an outcome + calling Worker APIs), or an error the
//! TUI renders inline.
//!
//! Keeping parse + registry here means `tui_chat.rs` only has to deal with
//! *executing* outcomes, which makes it easy to unit-test the command layer
//! without standing up a terminal.
//!
//! Scope: this wave ships the built-ins listed in
//! [`BUILTIN_COMMANDS`]. Popup-palette autocomplete is Tier 2 #9 and
//! deliberately not built yet — a plain-text `/help` is sufficient for
//! discoverability.

use camino::Utf8PathBuf;

use super::conversation::Agent;

/// A successfully parsed `/name args...` line. See [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Command name without the leading `/`. Lowercased by the parser.
    pub name: String,
    /// Everything after the first whitespace, trimmed. Empty when the user
    /// typed no arguments (e.g. `/help`).
    pub args: String,
}

/// Parse a chat-input line into a [`ParsedCommand`].
///
/// Returns `Some` only when `input` starts with `/` AND the first token is a
/// valid command shape: letters, digits, or hyphens. Deliberately rejects:
///
/// * bare `/` or `/ ` (no command name),
/// * leading whitespace before `/`,
/// * names with slashes or special chars (e.g. `/foo/bar`),
/// * the `?` shorthand is accepted as an alias for `help`.
///
/// Returning `None` means "not a slash command; send to the backend".
pub fn parse(input: &str) -> Option<ParsedCommand> {
    // Exact startswith — we treat leading whitespace as *not* a command so
    // users can paste/quote code blocks that begin with `/` without having
    // the parser eat them. Pi-mono does the same.
    let rest = input.strip_prefix('/')?;

    // Split on first whitespace.
    let (name_raw, args_raw) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };

    if name_raw.is_empty() {
        return None;
    }

    // `?` shorthand → help.
    if name_raw == "?" {
        return Some(ParsedCommand {
            name: "help".to_string(),
            args: args_raw.to_string(),
        });
    }

    if !is_valid_name(name_raw) {
        return None;
    }

    Some(ParsedCommand {
        name: name_raw.to_ascii_lowercase(),
        args: args_raw.to_string(),
    })
}

fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Severity tag for inline system messages the TUI renders in response to a
/// slash command. Maps to `Styles::success` / `warning` / `danger` in the
/// theme system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    /// Reserved for future commands that warn without erroring (e.g. a
    /// `/compact` run that succeeded but produced a trivial result). No
    /// built-in emits it yet; kept so the styling path is wired up for when
    /// extension commands arrive.
    #[allow(dead_code)]
    Warning,
    Error,
}

/// The built-in command kinds the TUI knows how to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Hotkeys,
    New,
    Resume,
    Compact,
    Copy,
    Export,
    Handoff,
    Focus,
    Quit,
}

/// What the TUI should do after a slash command is parsed + looked up.
///
/// The TUI (which owns Worker / UiState) is the only thing that can actually
/// perform these actions, so the registry returns intent and the TUI performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashOutcome {
    /// Consumed silently (no-op).
    #[allow(dead_code)]
    Consumed,
    /// Render this message inline as a system line.
    ShowMessage(String, Severity),
    /// Show the help list.
    ShowHelp,
    /// Show the hotkey list.
    ShowHotkeys,
    /// Clear the current conversation.
    ClearConversation,
    /// Open the session picker.
    RequireSessionPick,
    /// Invoke `Worker::compact_gpt_history` and report the result.
    Compact,
    /// Copy the last assistant message to the system clipboard.
    Copy,
    /// Render the current conversation to a self-contained HTML file.
    /// `path` is `None` when the user typed `/export` with no argument — the
    /// TUI defaults to `<conversation-dir>/export.html` in that case.
    Export { path: Option<Utf8PathBuf> },
    /// Rotate focus + auto-handoff last assistant turn to this agent.
    Handoff(Agent),
    /// Rotate focus only (no handoff).
    Focus(Agent),
    /// Exit the chat TUI.
    Quit,
}

/// A single entry in the built-in registry. The registry is a flat Vec so
/// `/help` can render it in declaration order, which is the order users see
/// in pi-mono too.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: SlashCommand,
    /// Human-readable args hint for `/help` (e.g. `"claude | gpt | codex"`).
    pub args_hint: &'static str,
}

/// Static list of built-in commands. Keep this in sync with
/// [`SlashCommand`]. Order controls display order in `/help`.
pub const BUILTIN_COMMANDS: &[BuiltinEntry] = &[
    BuiltinEntry {
        name: "help",
        description: "Show this command list.",
        kind: SlashCommand::Help,
        args_hint: "",
    },
    BuiltinEntry {
        name: "hotkeys",
        description: "Show keyboard shortcuts.",
        kind: SlashCommand::Hotkeys,
        args_hint: "",
    },
    BuiltinEntry {
        name: "new",
        description: "Start a new conversation (clears history + session ids).",
        kind: SlashCommand::New,
        args_hint: "",
    },
    BuiltinEntry {
        name: "resume",
        description: "Open the session picker and switch to a saved conversation.",
        kind: SlashCommand::Resume,
        args_hint: "",
    },
    BuiltinEntry {
        name: "compact",
        description: "Compact the GPT replay buffer by summarizing older turns.",
        kind: SlashCommand::Compact,
        args_hint: "",
    },
    BuiltinEntry {
        name: "copy",
        description: "Copy the last assistant message to the clipboard.",
        kind: SlashCommand::Copy,
        args_hint: "",
    },
    BuiltinEntry {
        name: "export",
        description: "Export the current conversation as a self-contained HTML file.",
        kind: SlashCommand::Export,
        args_hint: "[path]",
    },
    BuiltinEntry {
        name: "handoff",
        description: "Rotate focus AND hand off the last assistant turn to <agent>.",
        kind: SlashCommand::Handoff,
        args_hint: "claude | gpt | codex",
    },
    BuiltinEntry {
        name: "focus",
        description: "Rotate focus to <agent> without firing a handoff.",
        kind: SlashCommand::Focus,
        args_hint: "claude | gpt | codex",
    },
    BuiltinEntry {
        name: "quit",
        description: "Exit the chat TUI.",
        kind: SlashCommand::Quit,
        args_hint: "",
    },
];

/// Lookup + validation surface for the built-in registry.
pub struct CommandRegistry {
    entries: &'static [BuiltinEntry],
}

impl CommandRegistry {
    /// Registry populated with the shipping built-ins.
    pub fn builtins() -> Self {
        Self {
            entries: BUILTIN_COMMANDS,
        }
    }

    /// Read-only access to all entries (used by `/help` rendering).
    #[allow(dead_code)]
    pub fn entries(&self) -> &'static [BuiltinEntry] {
        self.entries
    }

    /// Case-insensitive name lookup (aliases resolved by [`parse`] already).
    pub fn find(&self, name: &str) -> Option<&BuiltinEntry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}

/// Resolve a [`ParsedCommand`] against the built-in registry to an outcome.
///
/// This is the pure-logic core of dispatch: given a parsed command, decide
/// what the TUI should do. It never performs IO or mutates state; argument
/// validation that depends on runtime state (e.g. "no assistant message to
/// copy") is handled by the TUI at execution time.
pub fn resolve(cmd: &ParsedCommand, registry: &CommandRegistry) -> SlashOutcome {
    let Some(entry) = registry.find(&cmd.name) else {
        return SlashOutcome::ShowMessage(
            format!("/{}: unknown command. Try /help.", cmd.name),
            Severity::Error,
        );
    };

    match entry.kind {
        SlashCommand::Help => SlashOutcome::ShowHelp,
        SlashCommand::Hotkeys => SlashOutcome::ShowHotkeys,
        SlashCommand::New => SlashOutcome::ClearConversation,
        SlashCommand::Resume => SlashOutcome::RequireSessionPick,
        SlashCommand::Compact => SlashOutcome::Compact,
        SlashCommand::Copy => SlashOutcome::Copy,
        SlashCommand::Export => {
            let trimmed = cmd.args.trim();
            let path = if trimmed.is_empty() {
                None
            } else {
                Some(Utf8PathBuf::from(trimmed))
            };
            SlashOutcome::Export { path }
        }
        SlashCommand::Quit => SlashOutcome::Quit,
        SlashCommand::Handoff => match parse_agent(&cmd.args) {
            Ok(a) => SlashOutcome::Handoff(a),
            Err(msg) => SlashOutcome::ShowMessage(msg, Severity::Error),
        },
        SlashCommand::Focus => match parse_agent(&cmd.args) {
            Ok(a) => SlashOutcome::Focus(a),
            Err(msg) => SlashOutcome::ShowMessage(msg, Severity::Error),
        },
    }
}

/// Parse an agent name. Accepts `claude`, `gpt`, `codex` (case-insensitive).
/// Returns a user-facing error string on miss.
fn parse_agent(raw: &str) -> Result<Agent, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("missing agent. available: claude, gpt, codex.".to_string());
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "claude" => Ok(Agent::Claude),
        "gpt" => Ok(Agent::Gpt),
        "codex" => Ok(Agent::Codex),
        other => Err(format!(
            "unknown agent {other:?}. available: claude, gpt, codex."
        )),
    }
}

// ────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_only() {
        let p = parse("/help").expect("parse");
        assert_eq!(p.name, "help");
        assert_eq!(p.args, "");
    }

    #[test]
    fn parse_name_with_args() {
        let p = parse("/handoff gpt").expect("parse");
        assert_eq!(p.name, "handoff");
        assert_eq!(p.args, "gpt");
    }

    #[test]
    fn parse_trims_inner_whitespace() {
        let p = parse("/handoff    claude   ").expect("parse");
        assert_eq!(p.name, "handoff");
        assert_eq!(p.args, "claude");
    }

    #[test]
    fn parse_preserves_multiword_args() {
        let p = parse("/foo bar baz").expect("parse");
        assert_eq!(p.name, "foo");
        assert_eq!(p.args, "bar baz");
    }

    #[test]
    fn parse_lowercases_name() {
        let p = parse("/HELP").expect("parse");
        assert_eq!(p.name, "help");
    }

    #[test]
    fn parse_rejects_bare_slash() {
        assert!(parse("/").is_none());
        assert!(parse("/ ").is_none());
        assert!(parse("/\t").is_none());
    }

    #[test]
    fn parse_rejects_leading_whitespace() {
        // A line that starts with whitespace is not a slash command, even if
        // it contains "/help" after it.
        assert!(parse(" /help").is_none());
    }

    #[test]
    fn parse_rejects_no_slash() {
        assert!(parse("help").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn parse_rejects_invalid_name_chars() {
        // No slashes, dots, etc. in command names.
        assert!(parse("/foo/bar").is_none());
        assert!(parse("/foo.bar").is_none());
        assert!(parse("/foo!").is_none());
    }

    #[test]
    fn parse_accepts_hyphens_and_digits() {
        let p = parse("/gpt-4").expect("parse");
        assert_eq!(p.name, "gpt-4");

        let p = parse("/do-thing-2 x").expect("parse");
        assert_eq!(p.name, "do-thing-2");
        assert_eq!(p.args, "x");
    }

    #[test]
    fn parse_question_mark_aliases_to_help() {
        let p = parse("/?").expect("parse");
        assert_eq!(p.name, "help");
        assert_eq!(p.args, "");
    }

    #[test]
    fn registry_finds_builtins_case_insensitive() {
        let reg = CommandRegistry::builtins();
        assert!(reg.find("help").is_some());
        assert!(reg.find("HELP").is_some());
        assert!(reg.find("Compact").is_some());
    }

    #[test]
    fn registry_returns_none_for_unknown() {
        let reg = CommandRegistry::builtins();
        assert!(reg.find("nope").is_none());
    }

    #[test]
    fn resolve_known_commands() {
        let reg = CommandRegistry::builtins();

        let out = resolve(
            &ParsedCommand {
                name: "help".into(),
                args: "".into(),
            },
            &reg,
        );
        assert_eq!(out, SlashOutcome::ShowHelp);

        let out = resolve(
            &ParsedCommand {
                name: "quit".into(),
                args: "".into(),
            },
            &reg,
        );
        assert_eq!(out, SlashOutcome::Quit);

        let out = resolve(
            &ParsedCommand {
                name: "new".into(),
                args: "".into(),
            },
            &reg,
        );
        assert_eq!(out, SlashOutcome::ClearConversation);
    }

    #[test]
    fn resolve_unknown_produces_error_message() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "nonesuch".into(),
                args: "".into(),
            },
            &reg,
        );
        match out {
            SlashOutcome::ShowMessage(msg, sev) => {
                assert!(msg.contains("/nonesuch"), "got: {msg}");
                assert!(msg.contains("unknown"));
                assert_eq!(sev, Severity::Error);
            }
            other => panic!("expected ShowMessage(Error), got {other:?}"),
        }
    }

    #[test]
    fn resolve_handoff_parses_agents() {
        let reg = CommandRegistry::builtins();
        for (input, expected) in [
            ("claude", Agent::Claude),
            ("gpt", Agent::Gpt),
            ("codex", Agent::Codex),
            ("  GPT  ", Agent::Gpt), // whitespace + case-insensitive
        ] {
            let out = resolve(
                &ParsedCommand {
                    name: "handoff".into(),
                    args: input.into(),
                },
                &reg,
            );
            assert_eq!(out, SlashOutcome::Handoff(expected), "input={input:?}");
        }
    }

    #[test]
    fn resolve_focus_parses_agents() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "focus".into(),
                args: "codex".into(),
            },
            &reg,
        );
        assert_eq!(out, SlashOutcome::Focus(Agent::Codex));
    }

    #[test]
    fn resolve_handoff_missing_arg_is_error() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "handoff".into(),
                args: "".into(),
            },
            &reg,
        );
        match out {
            SlashOutcome::ShowMessage(msg, Severity::Error) => {
                assert!(msg.contains("missing agent") || msg.contains("available"));
            }
            other => panic!("expected error message, got {other:?}"),
        }
    }

    #[test]
    fn resolve_handoff_unknown_agent_is_error() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "handoff".into(),
                args: "frobnicator".into(),
            },
            &reg,
        );
        match out {
            SlashOutcome::ShowMessage(msg, Severity::Error) => {
                assert!(msg.to_lowercase().contains("frobnicator"));
                assert!(msg.contains("claude"));
            }
            other => panic!("expected error message, got {other:?}"),
        }
    }

    #[test]
    fn builtins_has_all_expected_commands() {
        let reg = CommandRegistry::builtins();
        for name in [
            "help", "hotkeys", "new", "resume", "compact", "copy", "export", "handoff", "focus",
            "quit",
        ] {
            assert!(reg.find(name).is_some(), "missing builtin: {name}");
        }
    }

    #[test]
    fn resolve_export_without_args_is_default_path() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "export".into(),
                args: "".into(),
            },
            &reg,
        );
        assert_eq!(out, SlashOutcome::Export { path: None });
    }

    #[test]
    fn resolve_export_with_path_arg_carries_path() {
        let reg = CommandRegistry::builtins();
        let out = resolve(
            &ParsedCommand {
                name: "export".into(),
                args: "  /tmp/out.html  ".into(),
            },
            &reg,
        );
        assert_eq!(
            out,
            SlashOutcome::Export {
                path: Some(Utf8PathBuf::from("/tmp/out.html"))
            }
        );
    }
}

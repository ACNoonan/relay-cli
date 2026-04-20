# relay

[![CI](https://github.com/ACNoonan/relay-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/ACNoonan/relay-cli/actions/workflows/ci.yml)

Multi-agent chat that holds one conversation across Claude Code, Codex, and GPT. Rotate between agents with a keystroke and the previous agent's response is handed off as the next agent's prompt. Per-agent session IDs keep full context when you swap back.

Also ships the original artifact-driven workflows for scripted review / test / commit / CI pipelines, preserved below.

## Install

```bash
cargo install --path .
```

Or build from source:

```bash
cargo build --release
# Binary at target/release/relay
```

## Quick start

```bash
relay init                    # initialize harness in the current repo
relay doctor                  # verify provider availability

relay chat                    # multi-agent chat TUI (fuzzy picker when prior sessions exist)
relay chat --new              # skip the picker, start fresh
relay chat --print "Summarize src/bridge/" --rotation claude,codex,gpt  # CI-friendly non-interactive
```

## Chat TUI

A single-pane TUI that holds one conversation across Claude, GPT, and Codex.

- **Same conversation throughout.** Claude's `--resume` session id and Codex's `exec resume` thread id are kept per-agent, so rotating away and back continues the same thread. GPT has no server-side session, so its history is replayed locally each turn (with automatic compaction above ~32k tokens).
- **Persistence.** Every turn → `.agent-harness/conversations/<uuid>/{conversation.json, transcript.md}`. Resume via the auto-opening picker, `/resume` inside chat, or `relay chat --resume <uuid>`.
- **`--bare` Claude.** Uses `claude -p --bare --output-format stream-json --verbose --include-partial-messages`, dropping ~50K-token auto-discovery to ~5K per turn.

### Slash commands

Type `/` in the input to open a fuzzy autocomplete popup:

| Command | Description |
|---------|-------------|
| `/help` or `/?` | List all commands |
| `/hotkeys` | Show keybinding reference |
| `/new` | Clear conversation, start a fresh uuid |
| `/resume` | Open fuzzy picker over saved conversations |
| `/compact` | Summarize older GPT history (auto-triggers above threshold) |
| `/copy` | Copy the last assistant message to the clipboard |
| `/export [path]` | Write a self-contained HTML file of the conversation |
| `/handoff <agent>` | Rotate to named agent **with** auto-handoff |
| `/focus <agent>` | Rotate to named agent **without** handoff |
| `/skills` | List loaded skills (handoff recipes) and any load errors |
| `/<skill-name> [args]` | Invoke a loaded skill |
| `/quit` | Exit cleanly |

### Skills — shareable handoff recipes

Drop a markdown file at `.agent-harness/skills/<name>.md` (project-local) or `~/.config/relay/skills/<name>.md` (global). Each loaded skill becomes a slash command.

Skill format:

```markdown
---
name: security-review
description: Claude implements, Codex security-reviews, GPT summarizes
rotation: [claude, codex, gpt]
prompts:
  codex: "Focus on injection, auth bypass, and data leakage. Cite line:col."
  gpt: "Summarize Codex's findings in 3 bullets by severity."
---

Optional markdown body prepended to the first agent's prompt as context.
```

Invoke with `/security-review <your input>`. Project-scope skills shadow global ones; shadows are reported in `/skills`. See `assets/skills/security-review.md` for a working example.

### Keybindings

**Rotation:**
- `Shift+Right` — rotate to next agent (`Claude → GPT → Codex`), auto-handoff
- `Shift+Left` — rotate to previous agent, auto-handoff
- `Tab` — rotate without handoff (consumed by autocomplete popup when open)

**Editor (emacs-style):**
- `Ctrl+A` / `Ctrl+E` — cursor to start / end
- `Left` / `Right` — move cursor
- `Ctrl+K` — kill to end of input
- `Ctrl+U` — kill to start of input
- `Ctrl+W` — kill previous word
- `Alt+D` — kill next word
- `Ctrl+Y` — yank (paste last kill)
- `Alt+Y` — yank-pop (cycle previous kills)
- `Ctrl+Z` — undo (coalesces typing bursts within 500ms)
- `Ctrl+R` — redo
- `Backspace` / `Delete` — delete char

**Autocomplete popup (when `/` is active):**
- `Up` / `Down` — navigate
- `Tab` — accept selected command
- `Space` — accept and append a space for args
- `Esc` — dismiss (input buffer preserved)

**Session:**
- `Enter` — submit input
- `Ctrl+N` — alias for `/new`
- `Ctrl+H` — toggle auto-handoff-on-rotate
- `PgUp` / `PgDn` / `Home` / `End` — scroll the conversation log
- `Esc` — clear input (when popup closed)
- `Ctrl+C` or `q` (with empty input) — quit

### Themes

Three built-in themes: `amber` (default, preserves the original look), `dark`, `light`.

```bash
RELAY_THEME=dark relay chat
```

Or set persistently in `.agent-harness/config.toml`:

```toml
[ui]
theme = "dark"
```

Custom themes: drop JSON files under `~/.config/relay/themes/<name>.json`. See `assets/themes/theme-schema.json` for the 31-token semantic palette.

### Markdown rendering

Agent messages render with:
- Headings, lists, block quotes, horizontal rules, links
- Inline `code` and fenced code blocks with **syntect syntax highlighting**
- Streaming partials render as plain text until turn completion, then switch to styled markdown (no mid-stream flicker)

Tables and footnotes parse but render as minimal placeholders in v1.

### Compaction (GPT)

GPT has no server-side session, so its replay buffer grows each turn. Once estimated tokens exceed `[bridge.compaction].trigger_tokens` (default 32000), older turns are summarized into a single Summary turn with a "covers N prior turns" marker. `/compact` triggers compaction manually.

```toml
[bridge.compaction]
trigger_tokens = 32000        # threshold to auto-compact
keep_recent_tokens = 8000     # keep at least this much of the tail
min_keep_turns = 4            # hard floor on kept turns
auto = true                   # disable to require manual /compact only
```

Env overrides: `RELAY_COMPACTION_TRIGGER_TOKENS`, `RELAY_COMPACTION_KEEP_TOKENS`, `RELAY_COMPACTION_AUTO`.

### Chat flags

- `--start-with claude|gpt|codex` — agent to focus at startup (default: `claude`)
- `--resume <uuid>` — rehydrate a prior conversation
- `--new` — skip the session picker, start fresh
- `--no-auto-handoff` — rotate without triggering a handoff
- `--system-prompt-file <path>` — custom system prompt for GPT
- `--claude-model`, `--claude-binary`, `--codex-binary`, `--gpt-model`
- `--print <prompt>` — non-interactive mode (see below)
- `--rotation <list>` — comma-separated agents for print mode
- `--format text|json` — print-mode output format

## Print mode (CI / scripting)

Run a rotation non-interactively. Each agent processes one turn, then hands off to the next left-to-right.

```bash
# Single-agent one-shot
relay chat --print "Summarize what's new in src/bridge/" --start-with claude

# Three-agent pipeline
relay chat --print "Implement a /redo command" --rotation claude,codex,gpt

# NDJSON event stream for tooling
relay chat --print "..." --rotation claude,codex --format json | jq
```

**Text mode** prints only the final agent's content to stdout; errors to stderr. **JSON mode** emits one JSON object per line: `{"type":"turn_start",...}`, `{"type":"turn_end",...}`, `{"type":"done",...}`, `{"type":"error",...}`. Exit 0 on success, non-zero on backend error. Conversations persist as usual, so `relay chat --resume <uuid>` can pick up a print-mode run interactively.

`--print` is mutually exclusive with `--resume`. `--rotation` and `--format` require `--print`.

## Other workflows

Relay's original artifact-driven workflows are preserved for scripted review / test / commit / CI pipelines.

```
You <-> Claude Code (interactive)
           |
           v
     relay capture
           |
           v
     relay review codex  -->  structured findings (JSON + markdown)
           |
           v
     Back to Claude  -->  implement fixes
           |
           v
     relay test run  -->  structured test results
           |
           v
     relay commit prepare  -->  commit proposal
           |
           v
     relay ci watch  -->  CI status polling
```

Relay is **artifact-first**: every handoff produces a saved manifest and result under `.agent-harness/`. Nothing is invisible memory — everything is a file you can inspect.

| Command | Description |
|---------|-------------|
| `relay init` | Initialize harness in current repo |
| `relay doctor` | Check providers, auth, and configuration |
| `relay chat [flags]` | Multi-agent chat TUI (or `--print` for non-interactive) |
| `relay session start [provider]` | Start a standalone provider session |
| `relay session list / show <id>` | Browse standalone sessions |
| `relay capture last-response / transcript` | Capture output as artifact |
| `relay review codex / history` | Structured code review |
| `relay test run` | Run configured test commands |
| `relay commit prepare` | Generate commit proposal from git state |
| `relay ci watch` | Poll CI/CD status via GitHub CLI |
| `relay e2e` | Run end-to-end tests |
| `relay artifacts list / show <id>` | Browse artifacts |
| `relay config show / edit` | Inspect / edit `.agent-harness/config.toml` |
| `relay logs` | View session logs |
| `relay bridge ...` | Deprecated alias — forwards to `relay chat` |

## Providers

| Provider | Modes | Notes |
|----------|-------|-------|
| **Claude Code** | Interactive, Chat | Primary chat backend. Vendor CLI handles auth. |
| **Codex** | Chat, Review, Test, Commit | Structured non-interactive tasks + chat participant. Requires `OPENAI_API_KEY`. |
| **GPT (OpenAI)** | Chat | Direct OpenAI API. Requires `OPENAI_API_KEY`. Local replay buffer compacts automatically. |
| **Cursor** | Review | Second-opinion review. Experimental. |
| **Shell** | Test, CI | Git, test runners, CI polling. |

Relay invokes provider CLIs directly and never normalizes auth across providers. Each provider handles its own authentication.

## Configuration

`.agent-harness/config.toml`:

```toml
[workspace]
harness_dir = ".agent-harness"

[ui]
theme = "amber"                 # amber | dark | light | <custom>

[bridge.compaction]
trigger_tokens = 32000
keep_recent_tokens = 8000
min_keep_turns = 4
auto = true

[providers.claude]
binary = "claude"
interactive_only = true

[providers.codex]
binary = "codex"
non_interactive_enabled = true

[roles.reviewer]
provider = "codex"
safety_mode = "read_only"

[roles.tester]
provider = "shell"
safety_mode = "workspace_write"
test_commands = ["cargo test"]
```

## Storage layout

All state is local under `.agent-harness/`:

```
.agent-harness/
├── config.toml
├── skills/                      # project-local handoff recipes (*.md)
├── conversations/
│   └── <uuid>/
│       ├── conversation.json    # versioned; current schema: v2
│       ├── transcript.md
│       └── export.html          # optional, from /export
├── sessions/
│   └── <uuid>/
│       ├── session.json
│       ├── stdout.log
│       └── stderr.log
├── handoffs/
│   └── <uuid>/
│       ├── handoff.json
│       ├── prompt.md
│       ├── result.json
│       └── result.md
├── artifacts/
│   └── <uuid>/
│       ├── manifest.json
│       └── <content file>
├── runs/
├── logs/
└── cache/
```

`conversation.json` is version-tagged; relay migrates older files on load via `src/storage/migrations.rs`.

Global scope (not per-repo):

```
~/.config/relay/
├── skills/<name>.md             # user-global handoff recipes
└── themes/<name>.json           # custom themes
```

## Safety model

Every role has a default safety level:

| Role | Default Safety | Description |
|------|---------------|-------------|
| Reviewer | `read_only` | Cannot modify workspace |
| Tester | `workspace_write` | May write test artifacts |
| Committer | `workspace_write` | Proposes commits (no auto-commit) |
| CI Watcher | `read_only` | Polls status only |
| E2E Runner | `workspace_write` | May write test artifacts |

Non-interactive Claude automation is disabled by default. `relay doctor` will warn if it's enabled.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D clippy::dbg_macro -D clippy::todo -D clippy::unimplemented
cargo test --all-targets
cargo build

relay -v doctor                 # verbose logging
```

### Contributor workflow

1. Fork and create a branch for your change.
2. Add or update tests when behavior changes.
3. Run the fmt / clippy / test commands above.
4. Open a PR with a short summary and test plan.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

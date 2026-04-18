# relay

[![CI](https://github.com/ACNoonan/relay-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/ACNoonan/relay-cli/actions/workflows/ci.yml)

A local agent harness CLI that orchestrates Claude Code, Codex, Cursor, and utility agents through their native CLIs.

Relay lets you work interactively in Claude Code, then hand off responses and transcripts to other agents for review, testing, commit prep, CI tracking, and end-to-end execution — all through a single CLI with local, inspectable state.

## Install

```bash
cargo install --path .
```

Or build from source:

```bash
cargo build --release
# Binary at target/release/relay
```

## Quick Start

```bash
# Initialize in your repo
relay init

# Check your setup
relay doctor

# Start an interactive Claude session
relay session start claude

# After working in Claude, capture the output
relay capture last-response --file response.md

# Send to Codex for review
relay review codex

# Multi-agent chat TUI (Claude / GPT-5.4 / Codex)
relay chat --prompt "Review and improve src/provider/claude.rs"

# Run tests
relay test run --command "cargo test"

# Check CI
relay ci watch

# Browse artifacts
relay artifacts list
```

## How It Works

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

Relay is **artifact-first**: every handoff between agents produces a saved manifest and result under `.agent-harness/`. Nothing is invisible memory — everything is a file you can inspect.

## Commands

| Command | Description |
|---------|-------------|
| `relay init` | Initialize harness in current repo |
| `relay doctor` | Check providers, auth, and configuration |
| `relay session start [provider]` | Start an interactive session (default: claude) |
| `relay session list` | List all sessions |
| `relay session show <id>` | Show session details |
| `relay capture last-response` | Capture last response as artifact |
| `relay capture transcript` | Capture full conversation transcript |
| `relay review codex` | Send artifact to Codex for code review |
| `relay review history` | Show past reviews |
| `relay test run` | Run configured test commands |
| `relay commit prepare` | Generate commit proposal from git state |
| `relay ci watch` | Poll CI/CD status via GitHub CLI |
| `relay e2e` | Run end-to-end tests |
| `relay artifacts list` | Browse all artifacts |
| `relay artifacts show <id>` | Display artifact content |
| `relay config show` | Print current configuration |
| `relay config edit` | Open config in `$EDITOR` |
| `relay logs` | View session logs |
| `relay chat [--prompt ...]` | Launch the multi-agent chat TUI (Claude / GPT-5.4 / Codex) |
| `relay bridge ...` | Deprecated alias — forwards to `relay chat` |

### Chat TUI

A single-pane TUI that holds one conversation across three agents. The active agent
is shown in the top chrome; rotate between them with a hotkey and the previous
agent's last response is automatically handed off as the next agent's prompt.

- **Same conversation throughout.** Claude's `--resume` session id and Codex's
  `exec resume` thread id are kept per-agent, so swapping away and back continues
  the same thread with full context. GPT has no server-side session, so its
  history is replayed locally each turn.
- **Persistence.** Every turn is saved to `.agent-harness/conversations/<uuid>/`
  as both `conversation.json` and a human-readable `transcript.md`. Resume with
  `relay chat --resume <uuid>`.
- **`--bare` Claude.** The backend invokes `claude -p --bare
  --output-format stream-json --verbose --include-partial-messages`, which drops
  the ~50K-token auto-discovery overhead to ~5K per turn.

#### Keybindings

- `Enter` — send typed input to the active agent
- `Shift+Right` — rotate to the next agent (`Claude → GPT → Codex → Claude`) and
  auto-handoff the last assistant response as the new prompt
- `Shift+Left` — rotate to the previous agent with auto-handoff
- `Tab` — rotate to the next agent **without** handoff (focus-only)
- `Ctrl+N` — clear the conversation and all per-agent session state
- `Ctrl+H` — toggle auto-handoff-on-rotate
- `PgUp` / `PgDn` / `Home` / `End` — scroll the conversation log
- `Esc` — clear the input
- `Ctrl+C` or `q` (with empty input) — quit

#### Flags

- `--start-with claude|gpt|codex` — agent to focus at startup (default: `claude`)
- `--no-auto-handoff` — rotate without triggering a handoff
- `--resume <uuid>` — rehydrate a prior conversation
- `--system-prompt-file <path>` — custom system prompt for GPT
- `--claude-model`, `--claude-binary`, `--codex-binary`, `--gpt-model`

## Providers

| Provider | Modes | Notes |
|----------|-------|-------|
| **Claude** | Interactive | Primary working surface. Non-interactive disabled by default. |
| **Codex** | Review, Test, Commit | Structured non-interactive tasks. Requires `OPENAI_API_KEY`. |
| **Cursor** | Review | Second-opinion review. Experimental. |
| **Shell** | Test, CI | Git, test runners, CI polling. |

Relay invokes provider CLIs directly and never normalizes auth across providers. Each provider handles its own authentication.

## Configuration

Configuration lives at `.agent-harness/config.toml`:

```toml
[workspace]
harness_dir = ".agent-harness"

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

## Storage Layout

All state is local under `.agent-harness/`:

```
.agent-harness/
├── config.toml
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

## Safety Model

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
# Format check
cargo fmt --all -- --check

# Lint
cargo clippy --all-targets -- -D clippy::dbg_macro -D clippy::todo -D clippy::unimplemented

# Run tests
cargo test --all-targets

# Build
cargo build

# Run with verbose logging
relay -v doctor
```

### Contributor Workflow

1. Fork and create a branch for your change.
2. Add or update tests when behavior changes.
3. Run `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D clippy::dbg_macro -D clippy::todo -D clippy::unimplemented`, and `cargo test --all-targets`.
4. Open a PR with a short summary and test plan.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

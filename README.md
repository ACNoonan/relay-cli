# relay

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
# Run tests
cargo test

# Build
cargo build

# Run with verbose logging
relay -v doctor
```

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

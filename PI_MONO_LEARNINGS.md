# Learnings from pi-mono → relay roadmap

Source: [`badlogic/pi-mono`](https://github.com/badlogic/pi-mono) — TypeScript agent toolkit. Cloned to `/tmp/pi-mono` for source review.

## Framing

pi-mono and relay diverge on the most load-bearing choice:

- **pi-mono owns the agent loop.** Its own tool runtime, LLM provider abstraction, TUI library, and a `pi-coding-agent` that competes with Claude Code.
- **relay brokers between vendor loops.** Captures and hands off across Claude Code / Codex / GPT — never replaces them.

The **multi-agent chat TUI** (`relay chat`, persisted to `.agent-harness/conversations/`) is relay's product. This roadmap pulls patterns from pi-mono that strengthen that product. Patterns that only make sense for an agent runtime (provider abstraction, tool registry, agent loop) are explicitly excluded.

---

## Tier 1 — high leverage, small surface

| # | Item | Pi source | Relay target | Status |
|---|------|-----------|--------------|--------|
| 1 | Slash commands in chat input | `coding-agent/src/core/slash-commands.ts` (38) | `src/bridge/slash.rs` + `src/bridge/tui_chat.rs` | **done** — `d9aa61f` |
| 2 | JSON theme system | `coding-agent/src/modes/interactive/theme/{theme.ts, dark.json, light.json, theme-schema.json}` | `src/tui/theme/` + `assets/themes/` | **done** — `d196342` |
| 3 | Markdown rendering w/ syntax highlight | `tui/src/components/markdown.ts` (852) | `src/bridge/markdown.rs` + `src/bridge/tui_chat.rs` | **done** — `d558ee1` |
| 4 | Compaction with summarization | `coding-agent/src/core/compaction/{compaction.ts (823), branch-summarization.ts (355)}` | `src/bridge/compaction.rs`, `src/bridge/conversation.rs`, `src/bridge/worker.rs` | **done** — `d196342` |
| 5 | Fuzzy session picker | `tui/src/fuzzy.ts` + `coding-agent/src/modes/interactive/components/session-selector.ts` (1010) | `src/bridge/session_picker.rs` | **done** — `d196342` |

### 1. Slash commands
Built-in registry: `/new`, `/resume`, `/compact`, `/copy`, `/export`, `/model`, `/hotkeys`, `/fork`, `/quit`, `/handoff <agent>`. Replaces "more hotkeys" as the growth path; gives discoverability via tab-completion.

### 2. JSON theme system
~30 semantic tokens (`accent`, `mdHeading`, `toolPendingBg`, `userMessageBg`, …) loaded from `~/.config/relay/themes/*.json`. Direct port of pi's schema. Ship `dark.json` + `light.json` defaults.

### 3. Markdown rendering
Multi-agent chat is mostly code — currently raw text. Use `pulldown-cmark` for parsing and `syntect` for code-block syntax highlighting in ratatui. Single biggest visual upgrade.

### 4. Compaction
Critical for the GPT path specifically: per relay's README, "GPT has no server-side session, so its history is replayed locally each turn." Unbounded growth. Add `/compact` slash command + auto-trigger above N tokens, scoped to the GPT replay buffer first. Claude/Codex resume IDs handle their own compaction server-side.

### 5. Fuzzy session picker
`relay chat --resume <uuid>` exists but no UI. `/resume` opens a fuzzy picker over conversation directory entries (sorted by mtime, search by title or first user message).

---

## Tier 2 — pattern-level

| # | Item | Pi source | Relay target |
|---|------|-----------|--------------|
| 6 | Central event bus | `coding-agent/src/core/event-bus.ts` (33) | replaces ad-hoc channels in `src/bridge/worker.rs` (463) |
| 7 | HTML export | `coding-agent/src/core/export-html/` | new module; `/export` command |
| 8 | Schema migrations | `coding-agent/src/migrations.ts` (314) | new `src/storage/migrations.rs`; versioned `conversation.json` |
| 9 | Editor primitives | `tui/src/{kill-ring.ts, undo-stack.ts, autocomplete.ts}` | input handling in `src/bridge/tui_chat.rs` — kill-ring, undo, `/` and `@` autocomplete |

---

## Tier 3 — bigger swings

| # | Item | Pi source | Notes |
|---|------|-----------|-------|
| 10 | Print mode | `coding-agent/src/modes/print-mode.ts` | `relay chat --print "prompt" --rotation claude,codex,gpt` for CI/scripts |
| 11 | Library/SDK crate split | the monorepo itself | carve `relay-bridge` + `relay-tui-chat` as separate crates; lets external tools embed the bridge |
| 12 | Skills / extensions | `core/skills.ts` (508) + `core/extensions/` (~3100) | shareable handoff recipes as markdown files. Big architectural commitment — only if community demand emerges |

---

## Explicit non-recommendations

- **pi-ai provider abstraction** — irrelevant, relay shells out to CLIs by design.
- **pi agent-loop / tool registry** (`agent/src/agent-loop.ts`, `core/tools/`) — irrelevant, relay doesn't run an agent.
- **`auth-storage.ts`** — relay correctly delegates auth to vendor CLIs; do not take this on.

---

## Architectural tradeoff to keep honest

Relay talks to vendor **CLIs**; pi talks to vendor **APIs**. Relay rides vendor improvements for free but is brittle to output-format changes (e.g. Claude's `stream-json` schema). Worth naming explicitly in any contributor docs.

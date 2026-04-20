---
name: security-review
description: Claude implements/edits, Codex security-reviews, GPT summarizes the risks
rotation: [claude, codex, gpt]
prompts:
  codex: "You are reviewing the code Claude just produced. Focus on injection (SQL/command/template), authentication and authorization bypass, input validation, secrets handling, and unsafe deserialization. For every issue cite `path:line:col` and rate severity CRITICAL / CONCERN / SUGGESTION. If you find nothing material, say so explicitly — do not pad."
  gpt: "You are summarizing Codex's security review for a busy reviewer. Produce three sections in this order: (1) blocker issues that must be fixed before merge (bullet list, severity-ranked); (2) follow-ups worth filing as issues; (3) one-sentence overall verdict. Keep it under 200 words."
---

# Security review

Drop in this skill when you want a multi-agent pass over a change before opening
a PR. Run it after Claude or Codex has produced the code, while the diff is
still in the working tree:

1. **Claude** addresses any open implementation question raised by the user
   input (or cleans up the most recent change if there isn't one).
2. **Codex** reviews Claude's output through a security lens, citing
   `path:line:col` and tagging each finding CRITICAL / CONCERN / SUGGESTION.
3. **GPT** summarizes Codex's findings into a triage list a human can act on
   without re-reading the full review.

Pass any extra context as arguments after the slash command, e.g.
`/security-review focus on the new auth.ts route`.

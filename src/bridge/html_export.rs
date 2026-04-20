//! Export the current chat conversation as a self-contained HTML file.
//!
//! Companion to [`super::persist::ConversationStore`]'s `transcript.md`. The
//! markdown transcript is great for diffing or for piping through `glow`, but
//! it loses the visual structure a teammate needs when picking up a handoff
//! conversation: agent identity, role colors, code-block fencing, and the
//! "this turn replaces N earlier turns" badges produced by
//! [`super::compaction`].
//!
//! This module renders the same [`Conversation`] domain object into a single
//! HTML file with all CSS inlined and no external assets. A teammate can save
//! the file and open it in any browser, anywhere — no internet, no toolchain.
//!
//! Output shape (kept deliberately small — the goal is "readable diff of the
//! TUI", not a full SPA like pi-mono's export):
//!
//! ```html
//! <!DOCTYPE html>
//! <html lang="en"><head>
//!   <meta charset="utf-8">
//!   <title>relay conversation — <short-uuid> — <date></title>
//!   <style>/* inline; theme colors injected as CSS custom properties */</style>
//! </head><body>
//!   <header><h1>relay conversation</h1><div class="meta">…</div></header>
//!   <main>
//!     <article class="turn turn-assistant agent-claude">…</article>
//!     <article class="turn turn-summary">…</article>
//!   </main>
//!   <footer>Exported by relay <version> at <iso>.</footer>
//! </body></html>
//! ```
//!
//! ## Design choices
//!
//! - **Markdown via `pulldown_cmark::html::push_html`.** Same parser the TUI
//!   uses (no new dep). Tables / strikethrough / task-lists / footnotes
//!   enabled. Tables that the TUI defers DO render here — HTML is the better
//!   medium for them anyway.
//! - **Fenced code blocks via `syntect::html::highlighted_html_for_string`.**
//!   `syntect` is already in the dependency tree (Tier 1 #3) with the `html`
//!   feature. Output is self-contained inline-styled spans and adds maybe
//!   2–3 KB per highlighted block; far cleaner than embedding a JS
//!   highlighter and lets the file remain truly offline. On unknown languages
//!   we fall back to `<pre><code>` with no inline styling — the surrounding
//!   theme rules still give it a code-block frame.
//! - **All non-markdown text is HTML-escaped by [`escape_html`].** Markdown
//!   content is escaped by `pulldown-cmark` itself.
//! - **Per-agent tinting** is done via three hardcoded muted accent colors
//!   (`agent-claude` / `agent-gpt` / `agent-codex`) layered behind any theme
//!   so multi-agent threads stay scannable even on themes that don't define
//!   per-agent palette tokens. (Adding new tokens was out of scope.)
//! - **No JS at all.** Self-contained means self-contained.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use camino::Utf8Path;
use pulldown_cmark::{
    html as cmark_html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd,
};
use ratatui::style::Color;
use syntect::{highlighting::ThemeSet, html::highlighted_html_for_string, parsing::SyntaxSet};

use super::conversation::{Conversation, Role, Turn, TurnStatus};
use crate::tui::theme::Theme;

/// Render `conversation` to an HTML string under the active `theme`.
///
/// Returning the string (rather than writing inline) makes the function trivial
/// to unit-test and lets callers choose where to land the bytes (TUI writes to
/// `<conversation-dir>/export.html`; CLI may write to a user-supplied path).
pub fn render_conversation_html(conversation: &Conversation, theme: &Theme) -> String {
    let mut out = String::with_capacity(8 * 1024);

    let title_id = short_uuid(&conversation.id.to_string());
    let title_date = conversation.updated_at.format("%Y-%m-%d");
    let title = format!("relay conversation — {title_id} — {title_date}");

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("  <meta charset=\"utf-8\">\n");
    out.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
    out.push_str("  <title>");
    out.push_str(&escape_html(&title));
    out.push_str("</title>\n  <style>\n");
    out.push_str(&render_stylesheet(theme));
    out.push_str("  </style>\n</head>\n<body>\n");

    render_header(&mut out, conversation);
    out.push_str("  <main>\n");
    for (idx, turn) in conversation.turns.iter().enumerate() {
        render_turn(&mut out, idx, turn);
    }
    out.push_str("  </main>\n");
    render_footer(&mut out);

    out.push_str("</body>\n</html>\n");
    out
}

/// Convenience wrapper: render and write to disk.
pub fn export_conversation(
    conversation: &Conversation,
    theme: &Theme,
    output_path: &Utf8Path,
) -> Result<()> {
    let html = render_conversation_html(conversation, theme);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("creating export parent dir {parent}"))?;
    }
    std::fs::write(output_path.as_std_path(), html)
        .with_context(|| format!("writing export to {output_path}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTML pieces.
// ---------------------------------------------------------------------------

fn render_header(out: &mut String, conv: &Conversation) {
    let agents: Vec<&'static str> = collect_agent_labels(conv);
    out.push_str("  <header>\n    <h1>relay conversation</h1>\n");
    out.push_str("    <div class=\"meta\">\n");
    write_meta(out, "id", &conv.id.to_string());
    write_meta(out, "created", &conv.created_at.to_rfc3339());
    write_meta(out, "updated", &conv.updated_at.to_rfc3339());
    write_meta(out, "agents", &agents.join(", "));
    write_meta(out, "turns", &conv.turns.len().to_string());
    if let Some(cid) = &conv.sessions.claude_session_id {
        write_meta(out, "claude session", cid);
    }
    if let Some(tid) = &conv.sessions.codex_thread_id {
        write_meta(out, "codex thread", tid);
    }
    out.push_str("    </div>\n  </header>\n");
}

fn write_meta(out: &mut String, label: &str, value: &str) {
    out.push_str("      <span><b>");
    out.push_str(&escape_html(label));
    out.push_str(":</b> ");
    out.push_str(&escape_html(value));
    out.push_str("</span>\n");
}

fn collect_agent_labels(conv: &Conversation) -> Vec<&'static str> {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    for t in &conv.turns {
        if matches!(t.role, Role::Assistant | Role::Handoff) {
            seen.insert(t.agent.label());
        }
    }
    if seen.is_empty() {
        seen.insert(conv.active_agent.label());
    }
    seen.into_iter().collect()
}

fn render_turn(out: &mut String, idx: usize, turn: &Turn) {
    let role_class = match turn.role {
        Role::User => "turn-user",
        Role::Assistant => "turn-assistant",
        Role::System => "turn-system",
        Role::Handoff => "turn-handoff",
    };
    let agent_class = match turn.agent {
        super::conversation::Agent::Claude => "agent-claude",
        super::conversation::Agent::Gpt => "agent-gpt",
        super::conversation::Agent::Codex => "agent-codex",
    };
    let summary_class = if turn.is_summary() {
        " turn-summary"
    } else {
        ""
    };

    let role_label = match turn.role {
        Role::User => "you".to_string(),
        Role::Assistant => turn.agent.label().to_ascii_lowercase(),
        Role::System if turn.is_summary() => "summary".to_string(),
        Role::System => "system".to_string(),
        Role::Handoff => format!("↪ handoff → {}", turn.agent.label().to_ascii_lowercase()),
    };

    let _ = writeln!(
        out,
        "    <article class=\"turn {role_class} {agent_class}{summary_class}\" id=\"turn-{idx}\">"
    );
    out.push_str("      <header class=\"turn-header\">\n");
    out.push_str("        <span class=\"role\">");
    out.push_str(&escape_html(&role_label));
    out.push_str("</span>\n");
    if let Some(n) = turn.summarized_turn_count {
        out.push_str("        <span class=\"badge\">covers ");
        let _ = write!(out, "{n}");
        out.push_str(" prior turns</span>\n");
    }
    if turn.status == TurnStatus::Error {
        out.push_str("        <span class=\"badge badge-error\">error</span>\n");
    } else if turn.status == TurnStatus::Streaming {
        out.push_str("        <span class=\"badge badge-warn\">interrupted</span>\n");
    }
    out.push_str("        <span class=\"timestamp\">");
    out.push_str(&escape_html(&turn.ts.to_rfc3339()));
    out.push_str("</span>\n");
    out.push_str("      </header>\n");

    out.push_str("      <div class=\"turn-body\">\n");
    out.push_str(&markdown_to_html(&turn.content));
    out.push_str("      </div>\n");
    out.push_str("    </article>\n");
}

fn render_footer(out: &mut String) {
    out.push_str("  <footer>\n    <p>Exported by relay ");
    out.push_str(env!("CARGO_PKG_VERSION"));
    out.push_str(" at ");
    out.push_str(&escape_html(&chrono::Utc::now().to_rfc3339()));
    out.push_str(".</p>\n  </footer>\n");
}

// ---------------------------------------------------------------------------
// Markdown → HTML, with syntect-highlighted fenced code blocks.
// ---------------------------------------------------------------------------

/// Render a markdown string to an HTML fragment.
///
/// Walks `pulldown-cmark` events ourselves (rather than feeding them straight
/// through `push_html`) so we can swap fenced-code blocks out for
/// syntect-highlighted HTML. Everything else passes through untouched.
fn markdown_to_html(md: &str) -> String {
    if md.trim().is_empty() {
        return String::new();
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(md, options);

    // Two-pass walker: when we see Start(CodeBlock(Fenced/Indented)), we
    // collect text events until matching End and then emit highlighted HTML.
    // Outside code blocks we forward events to push_html in batches, which
    // gives us correct rendering without rewriting list/table/quote logic.
    let mut buf = String::new();
    let mut pending: Vec<Event<'_>> = Vec::new();
    let mut code_lang: Option<String> = None;
    let mut code_text = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                // Flush anything accumulated before this code block.
                cmark_html::push_html(&mut buf, std::mem::take(&mut pending).into_iter());
                code_lang = Some(match kind {
                    CodeBlockKind::Fenced(lang) => lang.into_string(),
                    CodeBlockKind::Indented => String::new(),
                });
                code_text.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = code_lang.take().unwrap_or_default();
                buf.push_str(&render_code_block(&code_text, &lang));
                code_text.clear();
            }
            Event::Text(t) if code_lang.is_some() => {
                code_text.push_str(&t);
            }
            // Defang raw HTML — agents can (and do) emit `<script>` blocks
            // verbatim in their markdown. CommonMark normally passes raw HTML
            // through unchanged; for an export that lives untrusted on a
            // teammate's filesystem that's a footgun. Convert to Text events
            // so push_html escapes them as visible source instead of injecting
            // them into the document.
            Event::Html(s) | Event::InlineHtml(s) => {
                pending.push(Event::Text(CowStr::Boxed(s.into_string().into_boxed_str())));
            }
            other => {
                pending.push(other);
            }
        }
    }

    // Flush trailing non-code events.
    cmark_html::push_html(&mut buf, pending.into_iter());
    buf
}

/// Render a single fenced code block. Tries `syntect` first (when a known
/// language hint is present); falls back to a plain `<pre><code>` block when
/// the language is unknown or empty so we never silently lose user content.
fn render_code_block(code: &str, lang: &str) -> String {
    let assets = syntect_assets();
    let lang_trim = lang.trim();
    let syntax = if lang_trim.is_empty() {
        None
    } else {
        assets
            .syntaxes
            .find_syntax_by_token(lang_trim)
            .or_else(|| assets.syntaxes.find_syntax_by_name(lang_trim))
    };

    if let Some(syntax) = syntax {
        if let Ok(html) = highlighted_html_for_string(code, &assets.syntaxes, syntax, &assets.theme)
        {
            // syntect emits a complete `<pre>…</pre>` with inline styles.
            // Tag the language so users can style or filter further if they want.
            let lang_attr = escape_html(lang_trim);
            return format!("<div class=\"code-block\" data-lang=\"{lang_attr}\">{html}</div>\n");
        }
    }

    // Plain fallback. We escape the code text manually since pulldown-cmark
    // would normally do this for us through push_html; we routed around it.
    let escaped = escape_html(code);
    let lang_attr = if lang_trim.is_empty() {
        String::new()
    } else {
        format!(" data-lang=\"{}\"", escape_html(lang_trim))
    };
    let class = if lang_trim.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", escape_html(lang_trim))
    };
    format!("<pre class=\"code-block plain\"{lang_attr}><code{class}>{escaped}</code></pre>\n")
}

struct SyntectAssets {
    syntaxes: SyntaxSet,
    theme: syntect::highlighting::Theme,
}

fn syntect_assets() -> &'static SyntectAssets {
    use std::sync::OnceLock;
    static CELL: OnceLock<SyntectAssets> = OnceLock::new();
    CELL.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .or_else(|| themes.themes.values().next().cloned())
            .expect("syntect ships at least one theme");
        SyntectAssets { syntaxes, theme }
    })
}

// ---------------------------------------------------------------------------
// Stylesheet (theme-aware).
// ---------------------------------------------------------------------------

fn render_stylesheet(theme: &Theme) -> String {
    let mut css = String::with_capacity(2048);

    // CSS custom properties from theme tokens. Anything that doesn't resolve
    // (terminal-default `Color::Reset`) falls back to a neutral default so the
    // export still looks reasonable in a browser, which has no concept of "the
    // terminal's default color".
    css.push_str("    :root {\n");
    let bg = color_or(&theme.token("bg"), "#181820");
    let fg = color_or(&theme.token("text"), "#e6e6e6");
    let border = color_or(&theme.token("border"), "#3a3a48");
    let header_bg = color_or(&theme.token("headerBg"), "#1f1f28");
    let header_fg = color_or(&theme.token("headerFg"), &fg);
    let accent = color_or(&theme.token("accent"), "#8abeb7");
    let dim = color_or(&theme.token("dim"), "#7a7a86");
    let muted = color_or(&theme.token("muted"), "#9a9aa6");
    let user_bg = color_or(&theme.token("selectedBg"), "#2a2a36");
    let code_block = color_or(&theme.token("mdCodeBlock"), "#1c1c24");
    let code_block_border = color_or(&theme.token("mdCodeBlockBorder"), "#2c2c38");
    let md_heading = color_or(&theme.token("mdHeading"), &accent);
    let md_quote = color_or(&theme.token("mdQuote"), &dim);
    let md_quote_border = color_or(&theme.token("mdQuoteBorder"), &border);
    let md_link = color_or(&theme.token("mdLink"), "#5fafff");
    let md_link_url = color_or(&theme.token("mdLinkUrl"), &dim);
    let success = color_or(&theme.token("success"), "#7fbf7f");
    let warning = color_or(&theme.token("warning"), "#d7af5f");
    let error = color_or(&theme.token("error"), "#cf6679");

    let _ = writeln!(css, "      --bg: {bg};");
    let _ = writeln!(css, "      --fg: {fg};");
    let _ = writeln!(css, "      --border: {border};");
    let _ = writeln!(css, "      --header-bg: {header_bg};");
    let _ = writeln!(css, "      --header-fg: {header_fg};");
    let _ = writeln!(css, "      --accent: {accent};");
    let _ = writeln!(css, "      --dim: {dim};");
    let _ = writeln!(css, "      --muted: {muted};");
    let _ = writeln!(css, "      --user-bg: {user_bg};");
    let _ = writeln!(css, "      --code-bg: {code_block};");
    let _ = writeln!(css, "      --code-border: {code_block_border};");
    let _ = writeln!(css, "      --md-heading: {md_heading};");
    let _ = writeln!(css, "      --md-quote: {md_quote};");
    let _ = writeln!(css, "      --md-quote-border: {md_quote_border};");
    let _ = writeln!(css, "      --md-link: {md_link};");
    let _ = writeln!(css, "      --md-link-url: {md_link_url};");
    let _ = writeln!(css, "      --success: {success};");
    let _ = writeln!(css, "      --warning: {warning};");
    let _ = writeln!(css, "      --error: {error};");
    css.push_str("    }\n");

    css.push_str(STATIC_CSS);
    css
}

/// Static CSS rules — zero dependence on the theme. Theme-driven values come
/// in via `var(--…)` declared in [`render_stylesheet`].
const STATIC_CSS: &str = r#"
    * { box-sizing: border-box; }
    html, body { margin: 0; padding: 0; }
    body {
      background: var(--bg);
      color: var(--fg);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
      font-size: 14px;
      line-height: 1.55;
    }
    header, main, footer {
      max-width: 920px;
      margin: 0 auto;
      padding: 16px 24px;
    }
    header {
      border-bottom: 1px solid var(--border);
      background: var(--header-bg);
      color: var(--header-fg);
    }
    header h1 {
      margin: 0 0 8px 0;
      font-size: 1.25rem;
      letter-spacing: 0.02em;
    }
    header .meta {
      display: flex;
      flex-wrap: wrap;
      gap: 4px 16px;
      font-size: 0.85rem;
      color: var(--muted);
    }
    header .meta b { color: var(--fg); font-weight: 600; }
    main { padding-top: 8px; padding-bottom: 24px; }
    footer {
      border-top: 1px solid var(--border);
      color: var(--dim);
      font-size: 0.8rem;
      text-align: center;
    }
    .turn {
      margin: 16px 0;
      border: 1px solid var(--border);
      border-radius: 6px;
      overflow: hidden;
      background: var(--bg);
    }
    .turn-header {
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 6px 12px;
      background: var(--header-bg);
      border-bottom: 1px solid var(--border);
      font-size: 0.85rem;
    }
    .turn-header .role {
      font-weight: 700;
      letter-spacing: 0.04em;
      text-transform: lowercase;
      color: var(--accent);
    }
    .turn-header .timestamp {
      margin-left: auto;
      color: var(--dim);
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      font-size: 0.78rem;
    }
    .turn-header .badge {
      font-size: 0.72rem;
      padding: 1px 6px;
      border-radius: 999px;
      background: rgba(255,255,255,0.06);
      color: var(--muted);
      border: 1px solid var(--border);
    }
    .turn-header .badge-error { color: var(--error); border-color: var(--error); }
    .turn-header .badge-warn  { color: var(--warning); border-color: var(--warning); }
    .turn-body { padding: 12px 16px; }
    .turn-body > :first-child { margin-top: 0; }
    .turn-body > :last-child  { margin-bottom: 0; }

    /* Per-role accents. */
    .turn-user      .turn-header { background: var(--user-bg); }
    .turn-user      .turn-header .role { color: var(--fg); }
    .turn-system    .turn-header { background: var(--header-bg); }
    .turn-system    .turn-header .role { color: var(--muted); }
    .turn-handoff   .turn-header .role { color: var(--warning); }
    .turn-summary   { border-style: dashed; }
    .turn-summary   .turn-header .role { color: var(--warning); font-style: italic; }

    /* Per-agent tints — small left-edge stripe so multi-agent threads scan visually. */
    .turn-assistant.agent-claude { border-left: 4px solid #c89b6c; }
    .turn-assistant.agent-gpt    { border-left: 4px solid #74a892; }
    .turn-assistant.agent-codex  { border-left: 4px solid #8a90c9; }
    .turn-handoff.agent-claude   { border-left: 4px solid #c89b6c; }
    .turn-handoff.agent-gpt      { border-left: 4px solid #74a892; }
    .turn-handoff.agent-codex    { border-left: 4px solid #8a90c9; }

    /* Markdown elements inside .turn-body. */
    .turn-body h1, .turn-body h2, .turn-body h3,
    .turn-body h4, .turn-body h5, .turn-body h6 {
      color: var(--md-heading);
      margin: 1.1em 0 0.4em 0;
      line-height: 1.25;
    }
    .turn-body p { margin: 0.6em 0; }
    .turn-body a { color: var(--md-link); }
    .turn-body a:visited { color: var(--md-link); }
    .turn-body ul, .turn-body ol { margin: 0.4em 0 0.4em 1.4em; padding: 0; }
    .turn-body li { margin: 0.15em 0; }
    .turn-body blockquote {
      margin: 0.6em 0;
      padding: 0.2em 1em;
      border-left: 3px solid var(--md-quote-border);
      color: var(--md-quote);
      font-style: italic;
    }
    .turn-body hr {
      border: 0;
      border-top: 1px solid var(--border);
      margin: 1em 0;
    }
    .turn-body code {
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      font-size: 0.92em;
      padding: 1px 4px;
      border-radius: 3px;
      background: var(--code-bg);
      border: 1px solid var(--code-border);
    }
    /* Code blocks: <pre><code> from the plain fallback, OR a syntect-rendered
       <div class="code-block"><pre>…</pre></div>. Style the wrapper either way. */
    .turn-body pre {
      margin: 0.7em 0;
      padding: 10px 12px;
      background: var(--code-bg);
      border: 1px solid var(--code-border);
      border-radius: 4px;
      overflow-x: auto;
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      font-size: 0.88em;
      line-height: 1.45;
    }
    .turn-body pre code { background: transparent; border: 0; padding: 0; }
    .turn-body .code-block { margin: 0.7em 0; }
    .turn-body .code-block pre { margin: 0; }
    .turn-body table {
      border-collapse: collapse;
      margin: 0.7em 0;
      font-size: 0.92em;
    }
    .turn-body th, .turn-body td {
      border: 1px solid var(--border);
      padding: 4px 8px;
      text-align: left;
    }
    .turn-body th { background: var(--header-bg); }
"#;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn short_uuid(s: &str) -> String {
    s.chars().take(8).collect()
}

/// Convert a ratatui `Color` to a CSS color string. `Color::Reset` and
/// undefined colors return an empty `String` so callers can fall back via
/// [`color_or`].
fn ratatui_color_to_css(color: &Color) -> String {
    match color {
        Color::Reset => String::new(),
        Color::Black => "#000000".to_string(),
        Color::Red => "#cc0000".to_string(),
        Color::Green => "#4e9a06".to_string(),
        Color::Yellow => "#c4a000".to_string(),
        Color::Blue => "#3465a4".to_string(),
        Color::Magenta => "#75507b".to_string(),
        Color::Cyan => "#06989a".to_string(),
        Color::Gray => "#d3d7cf".to_string(),
        Color::DarkGray => "#555753".to_string(),
        Color::LightRed => "#ef2929".to_string(),
        Color::LightGreen => "#8ae234".to_string(),
        Color::LightYellow => "#fce94f".to_string(),
        Color::LightBlue => "#729fcf".to_string(),
        Color::LightMagenta => "#ad7fa8".to_string(),
        Color::LightCyan => "#34e2e2".to_string(),
        Color::White => "#eeeeec".to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => indexed_to_css(*i),
    }
}

fn color_or(color: &Color, fallback: &str) -> String {
    let s = ratatui_color_to_css(color);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// Convert a 256-color palette index to a CSS hex string. The 16-color block
/// (0..16) maps to the basic ANSI colors; 16..232 is the 6×6×6 RGB cube;
/// 232..256 is the 24-step grayscale ramp.
fn indexed_to_css(i: u8) -> String {
    // Standard xterm 16-color block.
    const ANSI_16: [&str; 16] = [
        "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
        "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if (i as usize) < ANSI_16.len() {
        return ANSI_16[i as usize].to_string();
    }
    if (16..232).contains(&i) {
        let n = i - 16;
        let r = n / 36;
        let g = (n % 36) / 6;
        let b = n % 6;
        let scale = |c: u8| if c == 0 { 0u8 } else { 55 + c * 40 };
        return format!("#{:02x}{:02x}{:02x}", scale(r), scale(g), scale(b));
    }
    // Grayscale ramp 232..255.
    let level = 8 + (i - 232) * 10;
    format!("#{level:02x}{level:02x}{level:02x}")
}

/// Minimal HTML escaper for non-markdown content (titles, agent labels,
/// timestamps, metadata). Markdown text is escaped by `pulldown-cmark`'s own
/// HTML emitter, so we never double-escape.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::conversation::{Agent, Conversation, Role, Turn, TurnStatus};
    use crate::tui::theme::Styles;

    fn theme() -> Theme {
        Styles::builtin_default().theme
    }

    #[test]
    fn escape_html_handles_specials() {
        assert_eq!(
            escape_html("<script>alert(\"x\")</script>"),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"
        );
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("it's"), "it&#39;s");
        // Non-special chars pass through.
        assert_eq!(escape_html("hello, world!"), "hello, world!");
    }

    #[test]
    fn render_escapes_unsafe_agent_label_and_title() {
        // Build a conversation containing a turn with a content section that
        // looks dangerous when rendered raw, and ensure it survives escaping.
        let mut conv = Conversation::new(Agent::Gpt, true);
        // The turn content is markdown — pulldown-cmark escapes HTML for us.
        conv.turns.push(Turn::new(
            Agent::Gpt,
            Role::User,
            "<script>nope()</script> & <b>bold</b>",
            TurnStatus::Complete,
        ));
        let html = render_conversation_html(&conv, &theme());
        assert!(
            !html.contains("<script>nope()</script>"),
            "raw script tag must be escaped"
        );
        assert!(html.contains("&lt;script&gt;"));

        // The title (built from the conversation UUID + date) is escaped.
        let title_segment = format!("relay conversation — {}", short_uuid(&conv.id.to_string()));
        assert!(
            html.contains(&escape_html(&title_segment)),
            "expected escaped title in output"
        );
    }

    #[test]
    fn export_round_trip_writes_complete_html() {
        let mut conv = Conversation::new(Agent::Claude, true);
        // Mixed content: prose, fenced code block, and a list.
        conv.turns.push(Turn::new(
            Agent::Claude,
            Role::User,
            "What does this do?\n\n```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```",
            TurnStatus::Complete,
        ));
        conv.turns.push(Turn::new(
            Agent::Claude,
            Role::Assistant,
            "It returns the **sum**:\n\n- input: two i32s\n- output: their sum\n",
            TurnStatus::Complete,
        ));

        let html = render_conversation_html(&conv, &theme());

        // Title contains the uuid (short form).
        let short = short_uuid(&conv.id.to_string());
        assert!(
            html.contains(&short),
            "title should contain conversation id; output:\n{}",
            &html[..html.len().min(400)]
        );

        // Each turn appears as its own <article>.
        let article_count = html.matches("<article").count();
        assert_eq!(article_count, conv.turns.len(), "one article per turn");

        // Fenced code block becomes either a <pre><code> (plain fallback) or
        // a syntect-highlighted <div class="code-block">. Either way we want
        // a <pre> wrapping monospace output.
        assert!(
            html.contains("<pre"),
            "fenced code block should produce a <pre> element"
        );

        // No external <link> stylesheet or <script src=…> references.
        assert!(
            !html.contains("<link "),
            "self-contained export must not reference external stylesheets"
        );
        assert!(
            !html.contains("<script src"),
            "self-contained export must not reference external scripts"
        );

        // CSS is inlined.
        assert!(html.contains("<style>"), "stylesheet should be inlined");

        // Footer mentions the relay version.
        assert!(html.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn summary_turn_renders_badge() {
        let mut conv = Conversation::new(Agent::Gpt, true);
        conv.turns.push(Turn::new(
            Agent::Gpt,
            Role::User,
            "first thing",
            TurnStatus::Complete,
        ));
        conv.turns
            .push(Turn::new_summary("rolled-up earlier work", 7));
        conv.turns.push(Turn::new(
            Agent::Gpt,
            Role::Assistant,
            "current reply",
            TurnStatus::Complete,
        ));

        let html = render_conversation_html(&conv, &theme());

        // The summary turn should be marked with the dedicated class …
        assert!(
            html.contains("turn-summary"),
            "summary turn should carry `turn-summary` class"
        );
        // … and render the "covers N prior turns" badge with the exact count.
        assert!(
            html.contains("covers 7 prior turns"),
            "summary badge must show the original turn count"
        );
        // The role label switches from "system" to "summary" for clarity.
        assert!(html.contains(">summary<"));
    }

    #[test]
    fn ratatui_rgb_is_serialized_as_hex() {
        assert_eq!(
            ratatui_color_to_css(&Color::Rgb(0x12, 0x34, 0xab)),
            "#1234ab"
        );
        assert_eq!(ratatui_color_to_css(&Color::Reset), "");
        assert_eq!(color_or(&Color::Reset, "#abcdef"), "#abcdef");
    }

    /// Sanity check on output size for a ~10-turn mixed conversation. Marked
    /// `#[ignore]` because it just prints — it's a measurement, not an
    /// assertion, and the absolute number depends on theme + syntect output
    /// which both shift across syntect updates.
    ///
    /// Run with: `cargo test --lib bridge::html_export -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure_sample_output_size() {
        let mut conv = Conversation::new(Agent::Claude, true);
        for i in 0..5 {
            conv.turns.push(Turn::new(
                Agent::Gpt,
                Role::User,
                format!("Question {i}: how do I write a function in rust?"),
                TurnStatus::Complete,
            ));
            conv.turns.push(Turn::new(
                Agent::Claude,
                Role::Assistant,
                format!(
                    "Here's example {i}:\n\n```rust\nfn add(a: i32, b: i32) -> i32 {{\n    a + b\n}}\n```\n\nNotes:\n\n- pure function\n- inferred return\n- *italic*, **bold**, `inline code`",
                ),
                TurnStatus::Complete,
            ));
        }
        let html = render_conversation_html(&conv, &theme());
        eprintln!(
            "sample export size: {} bytes ({} turns)",
            html.len(),
            conv.turns.len()
        );
    }

    #[test]
    fn export_to_disk_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut conv = Conversation::new(Agent::Claude, true);
        conv.turns.push(Turn::new(
            Agent::Claude,
            Role::Assistant,
            "hi",
            TurnStatus::Complete,
        ));
        let path =
            camino::Utf8PathBuf::from_path_buf(tmp.path().join("export.html")).expect("utf8 path");
        export_conversation(&conv, &theme(), &path).expect("export");
        let on_disk = std::fs::read_to_string(path.as_std_path()).expect("read back");
        assert!(on_disk.starts_with("<!DOCTYPE html>"));
        assert!(on_disk.contains("</html>"));
    }
}

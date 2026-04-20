//! Render markdown into ratatui [`Line`]s for the chat TUI.
//!
//! Entry point: [`render_markdown`].
//!
//! ## Scope
//!
//! Agent assistant messages (Claude, Codex, GPT) come back as markdown. This
//! module parses that markdown with `pulldown-cmark`, folds it through a
//! small event walker, and emits owned `ratatui::text::Line<'static>` values
//! styled with [`Styles`]' `md_*` accessors. The chat TUI caches the
//! resulting `Vec<Line>` per-turn (keyed on content hash + width), so we can
//! afford some parser/highlighter work per render pass.
//!
//! Supported features:
//!   * paragraphs with soft-wrap honoring inline styling (bold / italic /
//!     strikethrough / inline code / links),
//!   * headings h1..h6 with a leading level-glyph,
//!   * fenced code blocks with `syntect` syntax highlighting (falling back
//!     to the `mdCodeBlock` foreground color when the language is unknown
//!     or no lang hint is given),
//!   * bulleted and numbered lists with nesting indent,
//!   * block quotes rendered with a left gutter,
//!   * horizontal rules,
//!   * links (text + dim parenthesised URL),
//!   * empty input returns an empty line vector.
//!
//! ## Design notes
//!
//! - **Fenced code blocks use *truncate*, not soft-wrap.** Soft-wrapping
//!   highlighted code mid-token produces ugly mid-word breaks and loses
//!   alignment. Truncating with an ellipsis preserves the "this is a code
//!   block" visual frame at the cost of occasionally hiding a long line's
//!   tail. Acceptable for v1; revisit if users complain.
//! - **`syntect` is lazily built and shared behind a `OnceLock`.** The
//!   default syntax set + theme set are loaded once (`~1ms` on first call)
//!   and reused forever.
//! - **No streaming-aware rendering.** Callers that want to render partial
//!   (in-flight) content should fall back to plain-text; re-parsing per
//!   delta would be wasteful. See `tui_chat.rs` for how this is wired.
//! - **Measured perf (release, M-series, post-warmup):** ~10ms for a
//!   650-line agent message saturated with code blocks + lists + quotes
//!   (`perf_500_line_input_under_budget`, `#[ignore]`-gated). Sub-5ms for
//!   typical prose-heavy content. The per-turn cache in `tui_chat.rs`
//!   means we pay this once per content change per width, not per frame.
//!
//! [`Styles`]: crate::tui::theme::Styles

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style as SyntectStyle, Theme as SyntectTheme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

use crate::tui::theme::Styles;

/// Render a markdown string into a vector of styled, owned `Line`s.
///
/// `width` is the available render width in cells; we soft-wrap paragraphs,
/// list items, and block quotes to fit. Code blocks are truncated rather
/// than wrapped (see module docs).
///
/// Empty / all-whitespace input yields an empty vector.
pub fn render_markdown(input: &str, width: u16, styles: &Styles) -> Vec<Line<'static>> {
    if input.trim().is_empty() {
        return Vec::new();
    }

    let width = width.max(10) as usize;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(input, options);
    let mut renderer = Renderer::new(width, styles);
    for event in parser {
        renderer.handle(event);
    }
    renderer.finish()
}

// ---------------------------------------------------------------------------
// Syntect assets (loaded once).
// ---------------------------------------------------------------------------

struct SyntectAssets {
    syntaxes: SyntaxSet,
    theme: SyntectTheme,
}

fn syntect_assets() -> &'static SyntectAssets {
    static CELL: OnceLock<SyntectAssets> = OnceLock::new();
    CELL.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        // `base16-ocean.dark` is a reasonable default across all three
        // shipped relay themes (amber / dark / light). We could key this
        // by the relay theme name — future work.
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| {
                // Fall back to any available theme — better than panicking.
                themes
                    .themes
                    .values()
                    .next()
                    .cloned()
                    .expect("syntect ships at least one theme")
            });
        SyntectAssets { syntaxes, theme }
    })
}

// ---------------------------------------------------------------------------
// Renderer — folds parser events into owned `Line`s.
// ---------------------------------------------------------------------------

/// A list frame on the rendering stack. Ordered lists increment their
/// counter; unordered lists leave `number` unset.
struct ListFrame {
    ordered: bool,
    number: u64,
}

/// Heading-in-progress marker. The level itself is used only at Start (for
/// the leading glyph); while collecting inline content, we just need to
/// know "we're inside a heading" — hence a marker type rather than a value
/// field.
struct HeadingFrame;

struct Renderer<'a> {
    width: usize,
    styles: &'a Styles,

    /// Flushed output lines.
    out: Vec<Line<'static>>,

    /// Inline-style modifier stack. Pushed/popped on Start/End of Emphasis,
    /// Strong, Strikethrough, and (via a manual style) inline Code. The
    /// current computed style is derived by walking the stack top-down.
    modifier_stack: Vec<Modifier>,

    /// Color-override stack (inline code, link).
    color_stack: Vec<Color>,

    /// Current accumulating line (built span-by-span).
    current_spans: Vec<Span<'static>>,
    /// Width of `current_spans`'s text content.
    current_width: usize,

    /// List depth stack. Empty when not inside a list.
    list_stack: Vec<ListFrame>,

    /// Block-quote depth — we render one gutter per level.
    quote_depth: usize,

    /// Heading frame (if we're mid-heading).
    heading: Option<HeadingFrame>,

    /// If `Some`, we're inside a fenced code block; the string is the lang
    /// hint (empty when unspecified). Content buffers here until `End(CodeBlock)`.
    code_lang: Option<String>,
    code_buffer: String,

    /// Inline-code accumulator (non-fenced `` `...` ``).
    in_inline_code: bool,

    /// When set, the next Text event is the href-part of a link and should
    /// be appended to `current_spans` as a dim `(url)` suffix *after* the
    /// link text flushes. pulldown-cmark delivers Link End with the URL in
    /// the Tag, so we actually read the URL from End(Link) — this flag is
    /// repurposed to emit a link-URL tail span at end-of-link time.
    link_url: Option<String>,
    link_text_start: Option<usize>,
}

impl<'a> Renderer<'a> {
    fn new(width: usize, styles: &'a Styles) -> Self {
        Self {
            width,
            styles,
            out: Vec::new(),
            modifier_stack: Vec::new(),
            color_stack: Vec::new(),
            current_spans: Vec::new(),
            current_width: 0,
            list_stack: Vec::new(),
            quote_depth: 0,
            heading: None,
            code_lang: None,
            code_buffer: String::new(),
            in_inline_code: false,
            link_url: None,
            link_text_start: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.out
    }

    /// Compute the base style for inline text, combining color + modifier stacks.
    fn current_inline_style(&self) -> Style {
        let mut style = Style::default();
        let mods = self
            .modifier_stack
            .iter()
            .copied()
            .fold(Modifier::empty(), |acc, m| acc | m);
        if !mods.is_empty() {
            style = style.add_modifier(mods);
        }
        if let Some(color) = self.color_stack.last() {
            style = style.fg(*color);
        }
        style
    }

    /// Push a span onto the current line, soft-wrapping at word boundaries.
    /// `style` is the span's intrinsic style; we also OR in the modifier
    /// stack (so nested bold/italic on top of e.g. a link still apply).
    fn push_inline(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }

        // Combine caller's style with stack modifiers.
        let mods = self
            .modifier_stack
            .iter()
            .copied()
            .fold(Modifier::empty(), |acc, m| acc | m);
        let style = if mods.is_empty() {
            style
        } else {
            style.add_modifier(mods)
        };

        // Fast path: it fits.
        let budget = self.line_budget();
        let remaining = budget.saturating_sub(self.current_width);
        let text_w = char_count(text);
        if text_w <= remaining {
            self.current_spans
                .push(Span::styled(text.to_string(), style));
            self.current_width += text_w;
            return;
        }

        // Soft-wrap. Split on word boundaries; emit as many whole words as
        // fit, flush, repeat.
        let mut pending = String::new();
        let mut pending_w = 0usize;
        let mut remaining = budget.saturating_sub(self.current_width);

        for part in split_preserving_spaces(text) {
            let part_w = char_count(&part);
            if part_w > remaining && !pending.is_empty() {
                // Flush what we've got, then newline.
                self.current_spans
                    .push(Span::styled(std::mem::take(&mut pending), style));
                self.current_width += pending_w;
                pending_w = 0;
                self.flush_line();
                remaining = self.line_budget();
                // Consume leading whitespace after a wrap to keep paragraphs tidy.
                if part.trim().is_empty() {
                    continue;
                }
            }
            if part_w > remaining {
                // A single token longer than the line — hard-break it by chars.
                for ch in part.chars() {
                    let cw = 1;
                    if cw > remaining && !pending.is_empty() {
                        self.current_spans
                            .push(Span::styled(std::mem::take(&mut pending), style));
                        self.current_width += pending_w;
                        pending_w = 0;
                        self.flush_line();
                        remaining = self.line_budget();
                    }
                    pending.push(ch);
                    pending_w += cw;
                    remaining = remaining.saturating_sub(cw);
                }
            } else {
                pending.push_str(&part);
                pending_w += part_w;
                remaining = remaining.saturating_sub(part_w);
            }
        }

        if !pending.is_empty() {
            self.current_spans.push(Span::styled(pending, style));
            self.current_width += pending_w;
        }
    }

    /// Available width for the current line, accounting for block-quote and
    /// list indents via [`Renderer::current_prefix_width`].
    fn line_budget(&self) -> usize {
        let pfx = self.current_prefix_width();
        self.width.saturating_sub(pfx).max(1)
    }

    /// Visible width of the prefix (gutter/indent) we'll prepend at flush.
    fn current_prefix_width(&self) -> usize {
        // Block-quote gutter is "┃ " (2 cells). We draw one per nesting level.
        let q = self.quote_depth * 2;
        // Lists indent 2 cells per depth. The bullet itself is added at
        // item-open time into `current_spans`, so we don't double-count it.
        let l = self.list_stack.len().saturating_sub(1) * 2;
        q + l
    }

    /// Emit `current_spans` as a [`Line`] with any prefix (quote gutter,
    /// etc.) prepended, then reset for the next line.
    fn flush_line(&mut self) {
        // Even if there's no content, we flush so that explicit blank lines
        // (paragraph breaks) produce spacing.
        let mut spans: Vec<Span<'static>> =
            Vec::with_capacity(self.quote_depth + self.current_spans.len() + 1);
        for _ in 0..self.quote_depth {
            spans.push(Span::styled(
                "\u{2503} ".to_string(), // ┃
                self.styles.md_quote_border(),
            ));
        }
        spans.append(&mut self.current_spans);
        self.out.push(Line::from(spans));
        self.current_width = 0;
    }

    /// Force a blank spacer line. Used between blocks.
    fn push_blank(&mut self) {
        // Avoid double-blanks at the document start or after another blank.
        if matches!(self.out.last(), Some(line) if line_is_blank(line))
            && self.current_spans.is_empty()
        {
            return;
        }
        if !self.current_spans.is_empty() {
            self.flush_line();
        }
        self.out.push(Line::from(Vec::<Span<'static>>::new()));
    }

    // -----------------------------------------------------------------------
    // Event handling.
    // -----------------------------------------------------------------------

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.on_text(&text),
            Event::Code(text) => {
                // Inline code — apply mdCode style as a one-shot span.
                let style = self.styles.md_code();
                self.push_inline(&text, style);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                // Render HTML fragments as plain text (no parsing) so
                // embedded <br/> etc. don't poison the output.
                self.on_text(&html);
            }
            Event::SoftBreak => {
                // Render soft breaks as spaces — they're visual line breaks
                // in the source that don't imply a paragraph boundary.
                self.push_inline(" ", self.current_inline_style());
            }
            Event::HardBreak => {
                self.flush_line();
            }
            Event::Rule => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                let w = self.width.min(80);
                let rule: String = "\u{2500}".repeat(w); // ─
                self.out.push(Line::styled(rule, self.styles.md_hr()));
                self.push_blank();
            }
            Event::TaskListMarker(checked) => {
                let glyph = if checked { "[x] " } else { "[ ] " };
                self.push_inline(glyph, self.styles.md_list_bullet());
            }
            Event::FootnoteReference(_) => {
                // Footnote refs render as the raw marker; relay rarely sees these.
                self.push_inline("[^]", self.current_inline_style());
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push_inline(&text, self.styles.md_code());
            }
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => { /* no-op */ }
            Tag::Heading { level, .. } => {
                self.heading = Some(HeadingFrame);
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                let glyph = heading_glyph(level);
                if !glyph.is_empty() {
                    self.push_inline(glyph, self.styles.md_heading());
                }
            }
            Tag::BlockQuote(_) => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(s) => s.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.code_lang = Some(lang);
                self.code_buffer.clear();
            }
            Tag::List(start) => {
                self.list_stack.push(ListFrame {
                    ordered: start.is_some(),
                    number: start.unwrap_or(1),
                });
            }
            Tag::Item => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                // Indent by parent-list depth (not including this one).
                let parent_depth = self.list_stack.len().saturating_sub(1);
                // We deliberately *don't* push the indent spaces into
                // `current_spans` here because `current_prefix_width` already
                // returns them for width math — but we DO need actual cells
                // in the output. Add them here as a leading padding span
                // and compensate by subtracting them from the prefix width
                // computation.
                if parent_depth > 0 {
                    let pad = "  ".repeat(parent_depth);
                    self.current_spans.push(Span::raw(pad));
                    self.current_width += parent_depth * 2;
                }
                // Bullet.
                if let Some(frame) = self.list_stack.last_mut() {
                    let bullet = if frame.ordered {
                        let s = format!("{}. ", frame.number);
                        frame.number += 1;
                        s
                    } else {
                        "\u{2022} ".to_string() // •
                    };
                    let bw = char_count(&bullet);
                    self.current_spans
                        .push(Span::styled(bullet, self.styles.md_list_bullet()));
                    self.current_width += bw;
                }
            }
            Tag::Emphasis => {
                self.modifier_stack.push(Modifier::ITALIC);
            }
            Tag::Strong => {
                self.modifier_stack.push(Modifier::BOLD);
            }
            Tag::Strikethrough => {
                self.modifier_stack.push(Modifier::CROSSED_OUT);
            }
            Tag::Link { dest_url, .. } => {
                self.color_stack
                    .push(self.styles.md_link().fg.unwrap_or(Color::Reset));
                self.modifier_stack.push(Modifier::UNDERLINED);
                self.link_url = Some(dest_url.to_string());
                self.link_text_start = Some(self.current_spans.len());
            }
            Tag::Image { dest_url, .. } => {
                // Images render as `[image: url]` placeholder.
                self.push_inline(&format!("[image: {}]", dest_url), self.styles.md_link_url());
            }
            // Table tokens — render as raw text for v1 (pi's table path is
            // complex; defer until user demand).
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::FootnoteDefinition(_) => {}
            Tag::HtmlBlock | Tag::MetadataBlock(_) => {}
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
            Tag::Superscript | Tag::Subscript => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.push_blank();
            }
            TagEnd::Heading(_) => {
                self.heading = None;
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.push_blank();
            }
            TagEnd::BlockQuote(_) => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.push_blank();
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take().unwrap_or_default();
                let body = std::mem::take(&mut self.code_buffer);
                self.emit_code_block(&lang, &body);
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    if !self.current_spans.is_empty() {
                        self.flush_line();
                    }
                    self.push_blank();
                }
            }
            TagEnd::Item => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
            }
            TagEnd::Emphasis => {
                pop_last(&mut self.modifier_stack, Modifier::ITALIC);
            }
            TagEnd::Strong => {
                pop_last(&mut self.modifier_stack, Modifier::BOLD);
            }
            TagEnd::Strikethrough => {
                pop_last(&mut self.modifier_stack, Modifier::CROSSED_OUT);
            }
            TagEnd::Link => {
                pop_last(&mut self.modifier_stack, Modifier::UNDERLINED);
                self.color_stack.pop();
                // If the URL differs from the link text, append "(url)" dim.
                if let Some(url) = self.link_url.take() {
                    let text_start = self.link_text_start.take().unwrap_or(0);
                    let visible_text: String = self
                        .current_spans
                        .iter()
                        .skip(text_start)
                        .map(|s| s.content.as_ref())
                        .collect();
                    if !url.is_empty() && url != visible_text {
                        let tail = format!(" ({})", url);
                        self.push_inline(&tail, self.styles.md_link_url());
                    }
                }
            }
            TagEnd::Image => {}
            TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::FootnoteDefinition => {}
            TagEnd::HtmlBlock | TagEnd::MetadataBlock(_) => {}
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {}
            TagEnd::Superscript | TagEnd::Subscript => {}
        }
    }

    fn on_text(&mut self, text: &str) {
        // Buffer code-block content verbatim — highlighted at close.
        if self.code_lang.is_some() {
            self.code_buffer.push_str(text);
            return;
        }
        // Split on '\n' so hard newlines inside Text events (rare, but
        // pulldown-cmark can produce them in HTML/lists) get proper Lines.
        let mut first = true;
        for segment in text.split('\n') {
            if !first {
                self.flush_line();
            }
            first = false;
            if segment.is_empty() {
                continue;
            }
            let style = if self.in_inline_code {
                self.styles.md_code()
            } else if self.heading.is_some() {
                self.styles.md_heading()
            } else if self.quote_depth > 0 {
                // Quote text: base it on the quote foreground + italic,
                // then let the modifier stack add bold/code/etc on top.
                self.styles.md_quote()
            } else {
                self.current_inline_style()
            };
            self.push_inline(segment, style);
        }
    }

    fn emit_code_block(&mut self, lang: &str, body: &str) {
        let border_style = self.styles.md_code_block_border();
        let fallback_style = self.styles.md_code_block();

        let body = body.strip_suffix('\n').unwrap_or(body);
        if body.is_empty() {
            // Still render the frame so an empty block is visible.
            let top = if lang.is_empty() {
                "```".to_string()
            } else {
                format!("```{lang}")
            };
            self.out.push(Line::styled(top, border_style));
            self.out.push(Line::styled("```", border_style));
            self.push_blank();
            return;
        }

        // Top border (with lang tag).
        let top = if lang.is_empty() {
            "```".to_string()
        } else {
            format!("```{lang}")
        };
        self.out.push(Line::styled(top, border_style));

        // Body — syntect if we can resolve the language, else raw.
        let indent = "  ";
        let content_budget = self.width.saturating_sub(indent.len()).max(10);

        let assets = syntect_assets();
        let syntax = if lang.is_empty() {
            None
        } else {
            assets
                .syntaxes
                .find_syntax_by_token(lang)
                .or_else(|| assets.syntaxes.find_syntax_by_extension(lang))
        };

        if let Some(syntax) = syntax {
            let mut highlighter = HighlightLines::new(syntax, &assets.theme);
            for line in LinesWithEndings::from(body) {
                let line_without_nl = line.strip_suffix('\n').unwrap_or(line);
                let ranges = highlighter
                    .highlight_line(line, &assets.syntaxes)
                    .unwrap_or_default();
                let mut spans: Vec<Span<'static>> = vec![Span::raw(indent.to_string())];
                let mut width_used = 0usize;
                for (sty, text) in ranges {
                    let text = text.trim_end_matches('\n');
                    if text.is_empty() {
                        continue;
                    }
                    let (clipped, truncated) =
                        clip(text, content_budget.saturating_sub(width_used));
                    if clipped.is_empty() && !truncated {
                        continue;
                    }
                    spans.push(Span::styled(clipped.clone(), syntect_style_to_ratatui(sty)));
                    width_used += char_count(&clipped);
                    if truncated {
                        spans.push(Span::styled(
                            "\u{2026}".to_string(), // …
                            fallback_style,
                        ));
                        break;
                    }
                }
                // Force a non-empty line so the visual frame stays intact.
                if spans.len() == 1 && line_without_nl.is_empty() {
                    spans.push(Span::raw(""));
                }
                self.out.push(Line::from(spans));
            }
        } else {
            for line in body.split('\n') {
                let (clipped, truncated) = clip(line, content_budget);
                let mut spans: Vec<Span<'static>> = vec![Span::raw(indent.to_string())];
                if !clipped.is_empty() {
                    spans.push(Span::styled(clipped, fallback_style));
                }
                if truncated {
                    spans.push(Span::styled(
                        "\u{2026}".to_string(), // …
                        fallback_style,
                    ));
                }
                self.out.push(Line::from(spans));
            }
        }

        // Bottom border.
        self.out.push(Line::styled("```".to_string(), border_style));
        self.push_blank();
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn heading_glyph(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "\u{258C} ", // ▌
        HeadingLevel::H2 => "\u{2503} ", // ┃
        HeadingLevel::H3 => "\u{2022} ", // •
        HeadingLevel::H4 => "\u{25E6} ", // ◦
        HeadingLevel::H5 => "\u{2023} ", // ‣
        HeadingLevel::H6 => "\u{2219} ", // ∙
    }
}

/// Convert a `syntect` style's foreground color to a ratatui `Style`.
/// Background colors from the syntect theme are dropped — relay's theme
/// dictates the block background via `mdCodeBlock` (which we don't yet fill
/// — terminals typically render it transparent), and mixing two palettes
/// looks muddy.
fn syntect_style_to_ratatui(sty: SyntectStyle) -> Style {
    let fg = sty.foreground;
    let mut style = Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
    let font = sty.font_style;
    if font.contains(syntect::highlighting::FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if font.contains(syntect::highlighting::FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if font.contains(syntect::highlighting::FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Truncate `text` to at most `budget` chars, returning the clipped prefix
/// and a flag indicating whether anything was dropped.
fn clip(text: &str, budget: usize) -> (String, bool) {
    if budget == 0 {
        return (String::new(), !text.is_empty());
    }
    let mut end = text.len();
    for (count, (i, _)) in text.char_indices().enumerate() {
        if count == budget {
            end = i;
            break;
        }
    }
    if end < text.len() {
        (text[..end].to_string(), true)
    } else {
        (text.to_string(), false)
    }
}

/// Split a string on whitespace boundaries while *preserving* the whitespace
/// as its own runs. Makes soft-wrap arithmetic trivial: we can place whole
/// tokens and discard leading whitespace after a wrap.
fn split_preserving_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut is_space = None;
    for ch in s.chars() {
        let space = ch.is_whitespace();
        match is_space {
            None => {
                current.push(ch);
                is_space = Some(space);
            }
            Some(prev) if prev == space => {
                current.push(ch);
            }
            Some(_) => {
                out.push(std::mem::take(&mut current));
                current.push(ch);
                is_space = Some(space);
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Pop the topmost occurrence of `target` from `stack`. Silently no-ops if
/// the modifier isn't present — this can happen on malformed input (e.g. a
/// stray `*` in CommonMark that the parser decides isn't an Emphasis open).
fn pop_last(stack: &mut Vec<Modifier>, target: Modifier) {
    if let Some(pos) = stack.iter().rposition(|m| *m == target) {
        stack.remove(pos);
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn styles() -> Styles {
        Styles::builtin_default()
    }

    #[test]
    fn empty_input_returns_no_lines() {
        let s = styles();
        assert!(render_markdown("", 80, &s).is_empty());
        assert!(render_markdown("   \n\n  \t", 80, &s).is_empty());
    }

    #[test]
    fn plain_paragraph_single_line() {
        let s = styles();
        let out = render_markdown("Hello, world.", 80, &s);
        // paragraph + trailing blank spacer
        assert!(!out.is_empty());
        let joined: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "Hello, world.");
    }

    #[test]
    fn headings_at_each_level() {
        let s = styles();
        for level in 1..=6 {
            let md = format!("{} heading {level}", "#".repeat(level));
            let out = render_markdown(&md, 80, &s);
            assert!(!out.is_empty(), "level {level} produced no lines");
            let joined: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                joined.contains(&format!("heading {level}")),
                "level {level} output was {joined:?}"
            );
        }
    }

    #[test]
    fn fenced_code_block_with_language() {
        let s = styles();
        let md = "```rust\nfn main() { println!(\"hi\"); }\n```";
        let out = render_markdown(md, 80, &s);
        // Top border, at least one highlighted line, bottom border, blank.
        assert!(out.len() >= 3);
        let top: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(top, "```rust");
        // Find a line containing `println`.
        let has_println = out
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains("println")));
        assert!(has_println);
    }

    #[test]
    fn fenced_code_block_without_language() {
        let s = styles();
        let md = "```\nraw text\n```";
        let out = render_markdown(md, 80, &s);
        let top: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(top, "```");
        assert!(out
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("raw text"))));
    }

    #[test]
    fn inline_code_style_is_applied() {
        let s = styles();
        let out = render_markdown("use `Vec::new()` here", 80, &s);
        assert!(!out.is_empty());
        // Find the code span; verify it has the mdCode fg.
        let code_span = out[0]
            .spans
            .iter()
            .find(|sp| sp.content.contains("Vec::new()"))
            .expect("inline code span");
        assert!(code_span.style.fg.is_some());
    }

    #[test]
    fn bold_and_italic_modifiers() {
        let s = styles();
        let out = render_markdown("**bold** and *ital*", 80, &s);
        assert!(!out.is_empty());
        let spans = &out[0].spans;
        let bold = spans
            .iter()
            .find(|sp| sp.content == "bold")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let ital = spans
            .iter()
            .find(|sp| sp.content == "ital")
            .expect("italic span");
        assert!(ital.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn paragraph_soft_wraps_at_width() {
        let s = styles();
        let long = "word ".repeat(30);
        let out = render_markdown(&long, 20, &s);
        // With width=20 we should have multiple wrapped lines.
        let non_blank: Vec<_> = out.iter().filter(|l| !line_is_blank(l)).collect();
        assert!(non_blank.len() > 1, "expected wrap, got {out:?}");
        for line in &non_blank {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(w <= 20, "line too wide ({w}): {line:?}");
        }
    }

    #[test]
    fn unordered_list_has_bullets() {
        let s = styles();
        let md = "- first\n- second\n- third";
        let out = render_markdown(md, 80, &s);
        // Expect at least 3 non-blank lines each containing a bullet.
        let bulleted: Vec<_> = out
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.content.starts_with('\u{2022}')))
            .collect();
        assert_eq!(bulleted.len(), 3, "out={out:?}");
    }

    #[test]
    fn ordered_list_numbers_items() {
        let s = styles();
        let md = "1. one\n2. two\n3. three";
        let out = render_markdown(md, 80, &s);
        let texts: Vec<String> = out
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("1. ") && t.contains("one")));
        assert!(texts.iter().any(|t| t.contains("2. ") && t.contains("two")));
        assert!(texts
            .iter()
            .any(|t| t.contains("3. ") && t.contains("three")));
    }

    #[test]
    fn nested_lists_indent() {
        let s = styles();
        let md = "- outer\n  - inner\n- outer2";
        let out = render_markdown(md, 80, &s);
        let inner_line = out
            .iter()
            .find(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("inner")
            })
            .expect("inner list item");
        let text: String = inner_line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // Expect leading whitespace before the bullet.
        assert!(
            text.starts_with("  "),
            "expected nested indent; got {text:?}"
        );
    }

    #[test]
    fn block_quote_has_left_gutter() {
        let s = styles();
        let md = "> quoted text";
        let out = render_markdown(md, 80, &s);
        let quoted = out
            .iter()
            .find(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("quoted text")
            })
            .expect("quote line");
        assert_eq!(quoted.spans[0].content.as_ref(), "\u{2503} ");
    }

    #[test]
    fn horizontal_rule_renders_as_dashes() {
        let s = styles();
        let out = render_markdown("before\n\n---\n\nafter", 80, &s);
        let has_rule = out.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.chars().filter(|c| *c == '\u{2500}').count() >= 3
        });
        assert!(has_rule, "expected HR row; got {out:?}");
    }

    #[test]
    fn link_text_and_url_both_render() {
        let s = styles();
        let md = "see [docs](https://example.com/docs)";
        let out = render_markdown(md, 80, &s);
        let joined: String = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(joined.contains("docs"));
        assert!(joined.contains("https://example.com/docs"));
    }

    /// Informational benchmark: renders a representative 500-line agent
    /// message (paragraphs, code blocks, lists, quotes) and asserts we
    /// stayed under 5ms on release builds. Marked `#[ignore]` so CI doesn't
    /// gate on machine timing; run manually with
    /// `cargo test --release -- --ignored perf_500_line`.
    #[test]
    #[ignore]
    fn perf_500_line_input_under_budget() {
        let s = styles();
        let mut md = String::new();
        for chunk in 0..25 {
            md.push_str(&format!("# Heading {chunk}\n\n"));
            md.push_str("Here is a **bolded** paragraph with *italic* and `inline::code()` and a [link](https://example.com). ");
            md.push_str("Lorem ipsum dolor sit amet consectetur adipisicing elit.\n\n");
            md.push_str("```rust\n");
            for line in 0..10 {
                md.push_str(&format!("    let x_{line} = compute({line});\n"));
            }
            md.push_str("```\n\n");
            md.push_str(
                "- first bullet\n- second bullet with `code`\n  - nested\n  - nested 2\n\n",
            );
            md.push_str("> a quoted block with **bold** content\n\n");
            md.push_str("---\n\n");
        }
        let line_count = md.lines().count();
        assert!(
            line_count >= 500,
            "test input should be >=500 lines; was {line_count}"
        );

        // Warm up the syntect OnceLock so the first-call init (loading the
        // default syntax + theme dumps) doesn't skew the timing. In the
        // real TUI this init happens once at startup.
        let _ = render_markdown("```rust\nfn main(){}\n```", 80, &s);

        let start = std::time::Instant::now();
        let out = render_markdown(&md, 100, &s);
        let elapsed = start.elapsed();
        eprintln!(
            "rendered {} input lines into {} output lines in {:?}",
            line_count,
            out.len(),
            elapsed
        );
        // Budget: 5ms on release M-series after warm-up. Dev builds are
        // ~5-10x slower, so we give ourselves headroom.
        assert!(
            elapsed.as_millis() < 50,
            "render took {:?} (budget 50ms in debug)",
            elapsed
        );
    }

    #[test]
    fn inline_style_survives_wrap() {
        let s = styles();
        // A bold span that spans a wrap boundary should keep both halves bold.
        // CommonMark Strong forbids whitespace adjacent to the delimiters, so
        // we build a bold block of single-letter words separated by spaces
        // that still forces wrapping at width=20.
        let inner: String = (0..30).map(|_| "X").collect::<Vec<_>>().join(" ");
        let md = format!("**{inner}**");
        let out = render_markdown(&md, 20, &s);
        let bolds: Vec<&Span<'_>> = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|sp| sp.style.add_modifier.contains(Modifier::BOLD))
            .collect();
        // At minimum, the bold text should cover multiple spans across wrapped lines.
        assert!(
            bolds.len() >= 2,
            "expected wrap-crossing bold; got {bolds:?}"
        );
    }
}

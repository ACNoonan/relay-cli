//! Fuzzy-search picker over saved chat conversations.
//!
//! Renders a self-contained ratatui app inside an alternate screen: a search
//! input on top, a filtered/sorted list in the middle, a status line at the
//! bottom. Returns the selected conversation `Uuid` (or `None` for cancel/new).
//!
//! The same [`pick_session`] function is used by the chat-startup flow today
//! and is the planned target for a future `/resume` slash command (Wave B).
//! Because it runs its own enable_raw_mode/EnterAlternateScreen scope and
//! tears down cleanly before returning, callers can sequence it before or
//! after the main chat TUI without state collision.
//!
//! Fuzzy scoring is a direct port of `pi-mono/packages/tui/src/fuzzy.ts`:
//! subsequence match, lower score = better, with bonuses for word-boundary
//! and consecutive matches and penalties for gaps. We keep this in-tree to
//! avoid adding a dependency for ~80 lines of logic.
//!
//! ```text
//! ┌ Resume conversation ───────────────────────────────────┐
//! │ search: refactor wo|                                   │
//! ├────────────────────────────────────────────────────────┤
//! │ > [N] New conversation                                 │
//! │   2h  Claude  refactor worker channel handoff plumbing │
//! │   3d  Codex   wire compaction summary into replay buf  │
//! │   ...                                                  │
//! ├────────────────────────────────────────────────────────┤
//! │ Enter:open  N:new  Esc:cancel  ↑/↓ navigate            │
//! └────────────────────────────────────────────────────────┘
//! ```

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{fs, io, time::Duration};
use uuid::Uuid;

use super::conversation::{Agent, Conversation, Role};

/// Metadata extracted per conversation entry, used for both display and
/// fuzzy-match scoring.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: Uuid,
    pub updated_at: DateTime<Utc>,
    /// Best-effort title — first user message snippet, or "(no messages)".
    pub title: String,
    pub message_count: usize,
    /// Distinct agents that participated (label list, e.g. "Claude, Codex").
    pub agents: String,
}

impl SessionEntry {
    /// Text searched against by the fuzzy filter — title plus uuid prefix
    /// plus agent labels, so users can search for "claude" or paste a uuid
    /// fragment.
    pub fn search_haystack(&self) -> String {
        let mut out = String::with_capacity(self.title.len() + 64);
        out.push_str(&self.title);
        out.push(' ');
        out.push_str(&self.id.to_string());
        out.push(' ');
        out.push_str(&self.agents);
        out
    }
}

/// Open the picker. If the conversations directory is missing or empty,
/// returns `Ok(None)` immediately so callers can fall through to a fresh
/// chat without spinning up a TUI.
///
/// Returns:
/// * `Ok(Some(uuid))` — user selected an existing conversation.
/// * `Ok(None)` — user picked "New conversation", hit Esc, or there was
///   nothing to pick from.
pub async fn pick_session(harness_dir: &Utf8Path) -> Result<Option<Uuid>> {
    let entries = load_session_entries(harness_dir)?;
    if entries.is_empty() {
        return Ok(None);
    }

    // ratatui is not async-aware; run the (short) UI loop on a blocking
    // thread so we don't stall the tokio reactor.
    tokio::task::spawn_blocking(move || run_picker(entries))
        .await
        .context("session picker task join")?
}

// ── Loading ────────────────────────────────────────────────────────────────

/// List conversations under `<harness_dir>/conversations/`, sorted by mtime
/// descending. Skips entries whose JSON cannot be parsed (best-effort: we'd
/// rather show 9 of 10 sessions than fail-shut).
pub fn load_session_entries(harness_dir: &Utf8Path) -> Result<Vec<SessionEntry>> {
    let conv_dir = harness_dir.join("conversations");
    if !conv_dir.as_std_path().is_dir() {
        return Ok(Vec::new());
    }

    let mut out: Vec<SessionEntry> = Vec::new();
    for entry in fs::read_dir(conv_dir.as_std_path())
        .with_context(|| format!("reading conversations dir {conv_dir}"))?
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(n) => n,
            None => continue,
        };
        let id = match Uuid::parse_str(name) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let json_path = conv_dir.join(name).join("conversation.json");
        if !json_path.as_std_path().is_file() {
            continue;
        }
        let bytes = match fs::read(json_path.as_std_path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let conv: Conversation = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };
        out.push(entry_from_conversation(id, &conv));
    }

    // Newest first.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

fn entry_from_conversation(id: Uuid, conv: &Conversation) -> SessionEntry {
    let title = derive_title(conv);
    let agents = derive_agent_labels(conv);
    SessionEntry {
        id,
        updated_at: conv.updated_at,
        title,
        message_count: conv.turns.len(),
        agents,
    }
}

fn derive_title(conv: &Conversation) -> String {
    let raw = conv
        .turns
        .iter()
        .find(|t| t.role == Role::User)
        .map(|t| t.content.as_str())
        .unwrap_or("(no messages)");
    let cleaned: String = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(raw)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    truncate_chars(trimmed, 80)
}

fn derive_agent_labels(conv: &Conversation) -> String {
    let mut seen: Vec<Agent> = Vec::new();
    for t in &conv.turns {
        if !seen.contains(&t.agent) {
            seen.push(t.agent);
        }
    }
    if seen.is_empty() {
        seen.push(conv.active_agent);
    }
    seen.iter()
        .map(|a| a.label())
        .collect::<Vec<_>>()
        .join(", ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (count, ch) in s.chars().enumerate() {
        if count == max {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

// ── Fuzzy matching (port of pi/tui/src/fuzzy.ts) ───────────────────────────

/// Subsequence fuzzy match. Lower score = better. Returns `None` if any query
/// char cannot be matched in order against `text`.
///
/// Heuristics:
/// * Reward consecutive matches (escalating bonus).
/// * Reward matches at word boundaries (start, after `[ \-_./:]`).
/// * Penalize gaps between matches.
/// * Tiny tail-bias so earlier matches edge out later ones.
pub fn fuzzy_score(query: &str, text: &str) -> Option<f64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    score_pass(&q, &t).or_else(|| {
        // Pi also retries with letters/digits swapped for queries like "v2foo"
        // <-> "foov2"; cheap to mirror.
        let swapped = swap_alpha_numeric(&q)?;
        score_pass(&swapped, &t).map(|s| s + 5.0)
    })
}

fn score_pass(query: &[char], text: &[char]) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    if query.len() > text.len() {
        return None;
    }

    let mut qi = 0usize;
    let mut score = 0.0f64;
    let mut last_match: Option<usize> = None;
    let mut consecutive: i64 = 0;

    for (i, ch) in text.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if *ch != query[qi] {
            continue;
        }
        let is_boundary =
            i == 0 || matches!(text[i - 1], ' ' | '\t' | '\n' | '-' | '_' | '.' | '/' | ':');
        if last_match == Some(i.saturating_sub(1)) {
            consecutive += 1;
            score -= (consecutive * 5) as f64;
        } else {
            consecutive = 0;
            if let Some(prev) = last_match {
                score += ((i - prev - 1) * 2) as f64;
            }
        }
        if is_boundary {
            score -= 10.0;
        }
        score += i as f64 * 0.1;
        last_match = Some(i);
        qi += 1;
    }

    if qi < query.len() {
        None
    } else {
        Some(score)
    }
}

fn swap_alpha_numeric(query: &[char]) -> Option<Vec<char>> {
    if query.is_empty() {
        return None;
    }
    let split = query.iter().position(|c| c.is_ascii_digit());
    if let Some(idx) = split {
        if idx > 0 && query[..idx].iter().all(|c| c.is_ascii_alphabetic()) {
            let (a, b) = query.split_at(idx);
            if b.iter().all(|c| c.is_ascii_digit()) {
                let mut out = b.to_vec();
                out.extend_from_slice(a);
                return Some(out);
            }
        }
    }
    let split = query.iter().position(|c| c.is_ascii_alphabetic());
    if let Some(idx) = split {
        if idx > 0 && query[..idx].iter().all(|c| c.is_ascii_digit()) {
            let (a, b) = query.split_at(idx);
            if b.iter().all(|c| c.is_ascii_alphabetic()) {
                let mut out = b.to_vec();
                out.extend_from_slice(a);
                return Some(out);
            }
        }
    }
    None
}

/// Filter and sort entries by fuzzy match. Empty query returns input order
/// (which the caller has already mtime-sorted). Space-separated tokens are
/// AND-ed: every token must match.
pub fn fuzzy_filter(entries: &[SessionEntry], query: &str) -> Vec<SessionEntry> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return entries.to_vec();
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let mut scored: Vec<(f64, &SessionEntry)> = Vec::new();
    'outer: for e in entries {
        let hay = e.search_haystack();
        let mut total = 0.0f64;
        for tok in &tokens {
            match fuzzy_score(tok, &hay) {
                Some(s) => total += s,
                None => continue 'outer,
            }
        }
        scored.push((total, e));
    }
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(_, e)| e.clone()).collect()
}

// ── Picker UI ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerOutcome {
    Selected(Uuid),
    NewConversation,
    Cancelled,
}

struct PickerState {
    all: Vec<SessionEntry>,
    filtered: Vec<SessionEntry>,
    query: String,
    /// 0 = "New conversation" sentinel row, 1.. = filtered entries.
    cursor: usize,
}

impl PickerState {
    fn new(all: Vec<SessionEntry>) -> Self {
        let filtered = all.clone();
        Self {
            all,
            filtered,
            query: String::new(),
            cursor: 1, // skip "new conversation" by default
        }
    }

    fn rerank(&mut self) {
        self.filtered = fuzzy_filter(&self.all, &self.query);
        // Clamp cursor into valid range. Total rows = filtered + 1 sentinel.
        let max = self.filtered.len();
        if self.cursor > max {
            self.cursor = max;
        }
        if self.cursor == 0 && !self.filtered.is_empty() {
            // Default to first real entry when there's anything to pick.
            self.cursor = 1;
        }
    }

    fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.cursor + 1 <= self.filtered.len() {
            self.cursor += 1;
        }
    }

    fn page_up(&mut self, page: usize) {
        self.cursor = self.cursor.saturating_sub(page.max(1));
    }

    fn page_down(&mut self, page: usize) {
        let max = self.filtered.len();
        self.cursor = (self.cursor + page.max(1)).min(max);
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.filtered.len();
    }

    fn select_current(&self) -> PickerOutcome {
        if self.cursor == 0 {
            PickerOutcome::NewConversation
        } else if let Some(e) = self.filtered.get(self.cursor - 1) {
            PickerOutcome::Selected(e.id)
        } else {
            PickerOutcome::NewConversation
        }
    }
}

fn run_picker(entries: Vec<SessionEntry>) -> Result<Option<Uuid>> {
    enable_raw_mode().context("enabling raw mode for session picker")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen for picker")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating picker terminal")?;

    let mut state = PickerState::new(entries);

    let outcome = (|| -> Result<PickerOutcome> {
        loop {
            terminal.draw(|f| draw(f, &state))?;
            if !event::poll(Duration::from_millis(150))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // Ctrl+C / Ctrl+D always cancel cleanly.
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
            {
                return Ok(PickerOutcome::Cancelled);
            }
            match key.code {
                KeyCode::Esc => return Ok(PickerOutcome::Cancelled),
                KeyCode::Enter => return Ok(state.select_current()),
                KeyCode::Up => state.move_up(),
                KeyCode::Down => state.move_down(),
                KeyCode::PageUp => state.page_up(8),
                KeyCode::PageDown => state.page_down(8),
                KeyCode::Home => state.home(),
                KeyCode::End => state.end(),
                KeyCode::Backspace => {
                    state.query.pop();
                    state.rerank();
                }
                KeyCode::Char(c) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        // Reserve other Ctrl+X for future bindings; ignore for now.
                        continue;
                    }
                    state.query.push(c);
                    state.rerank();
                }
                _ => {}
            }
        }
    })();

    // Always tear down the terminal — even on error — so the parent shell
    // and the chat TUI that follows aren't left in raw mode.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    match outcome? {
        PickerOutcome::Selected(id) => Ok(Some(id)),
        PickerOutcome::NewConversation | PickerOutcome::Cancelled => Ok(None),
    }
}

fn draw(f: &mut Frame, state: &PickerState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // search box
            Constraint::Min(3),    // list
            Constraint::Length(2), // status
        ])
        .split(f.area());

    draw_search(f, chunks[0], state);
    draw_list(f, chunks[1], state);
    draw_status(f, chunks[2], state);
}

fn draw_search(f: &mut Frame, area: Rect, state: &PickerState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Resume conversation ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let line = Line::from(vec![
        Span::styled(" search: ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("█", Style::default().fg(Color::Cyan)),
    ]);
    let p = Paragraph::new(line).block(block);
    f.render_widget(p, area);
}

fn draw_list(f: &mut Frame, area: Rect, state: &PickerState) {
    let mut items: Vec<ListItem> = Vec::with_capacity(state.filtered.len() + 1);

    // Sentinel row.
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            "  [N] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "New conversation",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])));

    let now = Utc::now();
    for entry in &state.filtered {
        let age = format_age(now.signed_duration_since(entry.updated_at));
        let agents = if entry.agents.is_empty() {
            "?".to_string()
        } else {
            entry.agents.clone()
        };
        let line = Line::from(vec![
            Span::styled(format!("  {age:>4} "), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:<14} ", agents),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(
                format!("{:>3} msg ", entry.message_count),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(entry.title.clone(), Style::default().fg(Color::White)),
        ]);
        items.push(ListItem::new(line));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.cursor));
    f.render_stateful_widget(list, area, &mut list_state);
}

fn draw_status(f: &mut Frame, area: Rect, state: &PickerState) {
    let total = state.all.len();
    let shown = state.filtered.len();
    let summary = if state.query.is_empty() {
        format!(
            " {total} conversation{} ",
            if total == 1 { "" } else { "s" }
        )
    } else {
        format!(" {shown}/{total} match ")
    };
    let line = Line::from(vec![
        Span::styled(summary, Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::styled(
            "  Enter:open  Esc:cancel  ↑/↓:move  PgUp/PgDn  Home:[N]ew  ",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

fn format_age(dur: chrono::Duration) -> String {
    let secs = dur.num_seconds().max(0);
    if secs < 60 {
        return "now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format!("{weeks}w");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo");
    }
    format!("{}y", days / 365)
}

// Re-export the harness path constructor used by callers that want to share
// this module's path conventions instead of inlining their own.
pub fn default_harness_dir() -> Utf8PathBuf {
    Utf8PathBuf::from(".agent-harness")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(id_byte: u8, title: &str, mins_ago: i64, agents: &str) -> SessionEntry {
        let updated = Utc.timestamp_opt(1_700_000_000 - mins_ago * 60, 0).unwrap();
        SessionEntry {
            id: Uuid::from_bytes([id_byte; 16]),
            updated_at: updated,
            title: title.to_string(),
            message_count: 1,
            agents: agents.to_string(),
        }
    }

    #[test]
    fn fuzzy_score_subsequence_matches() {
        // Subsequence in order matches even with gaps.
        assert!(fuzzy_score("rwc", "refactor worker channel").is_some());
        // Out-of-order does not.
        assert!(fuzzy_score("zoo", "refactor worker channel").is_none());
    }

    #[test]
    fn fuzzy_score_word_boundary_beats_midword() {
        // Same query, both with consecutive matches, but one starts at a
        // word boundary and the other starts mid-word.
        let boundary = fuzzy_score("ref", "refactor").unwrap();
        let mid = fuzzy_score("ref", "abrefxyz").unwrap();
        assert!(
            boundary < mid,
            "boundary score {boundary} should beat mid-word {mid}"
        );
    }

    #[test]
    fn fuzzy_score_consecutive_beats_scattered() {
        let consec = fuzzy_score("worker", "worker thread").unwrap();
        let scatter = fuzzy_score("worker", "wXoXrXkXeXr thread").unwrap();
        assert!(
            consec < scatter,
            "consecutive {consec} should beat scattered {scatter}"
        );
    }

    #[test]
    fn fuzzy_score_query_longer_than_text_fails() {
        assert!(fuzzy_score("worker", "wo").is_none());
    }

    #[test]
    fn fuzzy_filter_orders_best_first_and_drops_misses() {
        let entries = vec![
            entry(1, "compaction summary in replay buffer", 10, "GPT"),
            entry(2, "refactor worker channel handoff", 5, "Claude"),
            entry(3, "totally unrelated note", 15, "Codex"),
        ];
        let out = fuzzy_filter(&entries, "worker");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "refactor worker channel handoff");
    }

    #[test]
    fn fuzzy_filter_and_tokens() {
        let entries = vec![
            entry(1, "refactor worker channel", 5, "Claude"),
            entry(2, "worker test only", 10, "Codex"),
        ];
        // Both tokens must match.
        let out = fuzzy_filter(&entries, "worker channel");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "refactor worker channel");
    }

    #[test]
    fn fuzzy_filter_empty_query_preserves_input_order() {
        let entries = vec![
            entry(1, "first", 5, "Claude"),
            entry(2, "second", 10, "Codex"),
        ];
        let out = fuzzy_filter(&entries, "  ");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "first");
        assert_eq!(out[1].title, "second");
    }

    #[test]
    fn fuzzy_filter_searches_uuid_and_agents() {
        let entries = vec![entry(0xab, "anything", 5, "Claude")];
        // UUID prefix is in the haystack — fuzzy match against it.
        let out = fuzzy_filter(&entries, "abab");
        assert_eq!(out.len(), 1);
        let out = fuzzy_filter(&entries, "claude");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn picker_state_clamps_cursor_after_filter_shrinks() {
        let entries = vec![
            entry(1, "alpha worker", 5, "Claude"),
            entry(2, "beta worker", 10, "Codex"),
            entry(3, "gamma worker", 15, "GPT"),
        ];
        let mut s = PickerState::new(entries);
        s.cursor = 3;
        s.query = "alpha".to_string();
        s.rerank();
        // 1 sentinel + 1 hit = 2 valid rows (indices 0, 1)
        assert!(s.cursor <= 1);
    }

    #[test]
    fn picker_state_select_sentinel_and_entry() {
        let entries = vec![entry(0xcd, "thing", 5, "Codex")];
        let mut s = PickerState::new(entries);
        s.cursor = 0;
        assert_eq!(s.select_current(), PickerOutcome::NewConversation);
        s.cursor = 1;
        match s.select_current() {
            PickerOutcome::Selected(id) => assert_eq!(id, Uuid::from_bytes([0xcd; 16])),
            _ => panic!("expected selected"),
        }
    }

    #[test]
    fn derive_title_uses_first_user_message_first_nonblank_line() {
        use crate::bridge::conversation::{Turn, TurnStatus};
        let mut conv = Conversation::new(Agent::Claude, true);
        conv.append_turn(Turn::new(
            Agent::Claude,
            Role::User,
            "\n\nfirst real line\nsecond line",
            TurnStatus::Complete,
        ));
        let title = derive_title(&conv);
        assert_eq!(title, "first real line");
    }

    #[test]
    fn derive_title_truncates_with_ellipsis() {
        use crate::bridge::conversation::{Turn, TurnStatus};
        let mut conv = Conversation::new(Agent::Claude, true);
        let long = "x".repeat(200);
        conv.append_turn(Turn::new(
            Agent::Claude,
            Role::User,
            long,
            TurnStatus::Complete,
        ));
        let title = derive_title(&conv);
        let count = title.chars().count();
        assert!(count <= 81, "expected <=81 chars, got {count}");
        assert!(title.ends_with('…'));
    }

    #[test]
    fn derive_agents_lists_distinct_agents_in_order() {
        use crate::bridge::conversation::{Turn, TurnStatus};
        let mut conv = Conversation::new(Agent::Claude, true);
        conv.append_turn(Turn::new(
            Agent::Claude,
            Role::User,
            "a",
            TurnStatus::Complete,
        ));
        conv.append_turn(Turn::new(
            Agent::Claude,
            Role::Assistant,
            "b",
            TurnStatus::Complete,
        ));
        conv.append_turn(Turn::new(
            Agent::Codex,
            Role::Assistant,
            "c",
            TurnStatus::Complete,
        ));
        let labels = derive_agent_labels(&conv);
        assert_eq!(labels, "Claude, Codex");
    }

    #[test]
    fn load_session_entries_skips_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let out = load_session_entries(&path).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn load_session_entries_reads_real_conversations_sorted_newest_first() {
        use crate::bridge::conversation::{Turn, TurnStatus};
        use crate::bridge::persist::ConversationStore;

        let tmp = tempfile::tempdir().unwrap();
        let harness = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        // Mimic `relay init` enough that the store engages.
        std::fs::create_dir_all(harness.join("conversations").as_std_path()).unwrap();
        std::fs::write(harness.join("config.toml").as_std_path(), "").unwrap();

        let store = ConversationStore::open(Some(harness.clone()));
        assert!(store.is_enabled());

        let mut older = Conversation::new(Agent::Claude, true);
        older.append_turn(Turn::new(
            Agent::Claude,
            Role::User,
            "older one",
            TurnStatus::Complete,
        ));
        older.updated_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        store.save(&older).unwrap();

        let mut newer = Conversation::new(Agent::Codex, true);
        newer.append_turn(Turn::new(
            Agent::Codex,
            Role::User,
            "newer one",
            TurnStatus::Complete,
        ));
        newer.updated_at = Utc.timestamp_opt(1_700_001_000, 0).unwrap();
        store.save(&newer).unwrap();

        let out = load_session_entries(&harness).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "newer one");
        assert_eq!(out[1].title, "older one");
    }
}

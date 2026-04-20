//! Editor primitives for the chat input: kill-ring, undo/redo, slash autocomplete.
//!
//! These three small state machines live alongside the existing [`super::tui_chat`]
//! input buffer. They are deliberately narrow ports of pi-mono's equivalents
//! (`kill-ring.ts`, `undo-stack.ts`, `autocomplete.ts`) — we take the *semantics*,
//! not the 2k-line editor component that assembles them in pi.
//!
//! Scope (Tier 2 #9 of `PI_MONO_LEARNINGS.md`):
//!
//! * [`KillRing`]  — Emacs-style cut buffer with a ring of kills. Consecutive
//!   kills merge; yank pastes; yank-pop cycles older entries.
//! * [`UndoStack`] — Generic `Clone`-based undo/redo, with a 500 ms coalescing
//!   window so single-character typing doesn't flood the stack.
//! * [`SlashAutocomplete`] — Filters the [`super::slash::BUILTIN_COMMANDS`]
//!   list by prefix while the input buffer looks like an in-progress
//!   `/command` (no space yet).
//!
//! Intentional non-goals (Tier 3+): `@`-triggered file-path autocomplete,
//! multi-line editing, arbitrary-position paste-undo, regex search. Keep this
//! file boring.

use std::time::{Duration, Instant};

use super::slash::BUILTIN_COMMANDS;

// ─────────────────────────────────────────────────────────────────────────────
// KillRing
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum number of distinct kills we keep. Pi-mono doesn't cap; we do because
/// we're running in a long-lived TUI and bounded memory is cheap insurance. 60
/// is way more than any real Emacs session uses.
const KILL_RING_CAP: usize = 60;

/// Emacs-style kill-ring.
///
/// Two operations drive the state machine:
///
/// 1. **`kill(text)`** — records deleted text. If the *previous* recorded
///    action was also a kill (tracked via [`KillRing::mark_kill_boundary`] /
///    the internal `last_was_kill` flag) the new text is appended to the head
///    entry rather than pushed as a new entry. This matches Emacs: `C-k C-k`
///    on two consecutive lines yields one kill-ring entry with both lines.
///
/// 2. **`yank()` / `yank_pop()`** — `yank` returns the head entry without
///    mutating the ring; `yank_pop` rotates the ring so the second-most-recent
///    entry becomes the head, and returns the new head. `yank_pop` is only
///    meaningful immediately after a `yank`; the caller is responsible for
///    tracking that context (pi does the same — `last-was-yank` lives on the
///    editor, not the ring).
///
/// Any non-kill user action must call [`KillRing::mark_boundary`] so the next
/// kill starts a fresh entry instead of appending.
#[derive(Debug, Default, Clone)]
pub struct KillRing {
    /// Newest entry is at the *end* of the Vec (push/pop cheap). `yank()` looks
    /// at `ring.last()`.
    ring: Vec<String>,
    /// True iff the immediately-preceding edit action was a `kill`. Cleared by
    /// [`KillRing::mark_boundary`].
    last_was_kill: bool,
}

impl KillRing {
    #[allow(dead_code)] // kept alongside Default for API symmetry + tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `text` as a kill.
    ///
    /// * `prepend = true` → the text was deleted *before* the cursor
    ///   (Ctrl+U / Ctrl+W). Appends to the head entry with the new text at the
    ///   front, so `"hello "` killed then `"world"` killed yields `"hello world"`.
    /// * `prepend = false` → the text was deleted *after* the cursor
    ///   (Ctrl+K / Alt+D). Appends to the head entry with the new text at the
    ///   end.
    ///
    /// Empty `text` is a no-op (still sets `last_was_kill = true`, matching pi).
    pub fn kill(&mut self, text: &str, prepend: bool) {
        if text.is_empty() {
            self.last_was_kill = true;
            return;
        }
        if self.last_was_kill && !self.ring.is_empty() {
            let last = self.ring.last_mut().expect("non-empty");
            if prepend {
                let mut joined = String::with_capacity(text.len() + last.len());
                joined.push_str(text);
                joined.push_str(last);
                *last = joined;
            } else {
                last.push_str(text);
            }
        } else {
            self.ring.push(text.to_string());
            if self.ring.len() > KILL_RING_CAP {
                // Drop oldest (front).
                self.ring.remove(0);
            }
        }
        self.last_was_kill = true;
    }

    /// Signal that a non-kill action happened (cursor move, insertion, undo, …).
    /// The next [`KillRing::kill`] will start a fresh entry.
    pub fn mark_boundary(&mut self) {
        self.last_was_kill = false;
    }

    /// Return the most recent kill without modifying the ring. `None` when the
    /// ring is empty.
    pub fn yank(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Cycle to the previous entry and return it. With ≥2 entries, moves the
    /// current head to the front so the second-most-recent becomes the new
    /// head. With 0 or 1 entry, returns whatever `yank()` would return (no-op).
    pub fn yank_pop(&mut self) -> Option<&str> {
        if self.ring.len() > 1 {
            let last = self.ring.pop().expect("non-empty");
            self.ring.insert(0, last);
        }
        self.yank()
    }

    #[allow(dead_code)] // exposed for tests and future code paths.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UndoStack
// ─────────────────────────────────────────────────────────────────────────────

/// Max snapshots we keep on either the undo or redo side. Pi-mono doesn't cap
/// explicitly; we do because chat prompts are short and 200 entries is already
/// hundreds of keystrokes.
const UNDO_STACK_CAP: usize = 200;

/// Window in which two consecutive [`UndoStack::push_edit`] calls are
/// considered a single "typing burst" and coalesce into one undo entry.
const COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// Generic undo/redo stack.
///
/// Holds clones of the full state (for relay, that's the input buffer + cursor
/// position — `String` + `usize` pair). Any push to the undo side clears the
/// redo side; pop-to-undo moves an entry across, pop-to-redo moves it back.
///
/// Coalescing is only applied through [`UndoStack::push_edit`]; bulk/boundary
/// edits (paste, kill, yank, newline) should call [`UndoStack::push`] to force
/// a distinct entry.
#[derive(Debug)]
pub struct UndoStack<T: Clone> {
    undo: Vec<T>,
    redo: Vec<T>,
    /// Timestamp of the last `push_edit` — used for the 500 ms coalesce window.
    last_push: Option<Instant>,
}

impl<T: Clone> Default for UndoStack<T> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last_push: None,
        }
    }
}

impl<T: Clone> UndoStack<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new snapshot as a *hard* boundary. Always distinct. Use for
    /// paste, kill, yank, newline, explicit break actions.
    pub fn push(&mut self, state: T) {
        self.redo.clear();
        self.undo.push(state);
        if self.undo.len() > UNDO_STACK_CAP {
            self.undo.remove(0);
        }
        self.last_push = None; // force next push_edit to NOT coalesce
    }

    /// Push an incremental edit. If the last `push_edit` happened within
    /// [`COALESCE_WINDOW`], the top of the undo stack is replaced with
    /// `state` instead of growing the stack. This keeps single-char typing
    /// from exploding the stack without losing the pre-burst checkpoint.
    ///
    /// Uses a caller-supplied `now` for testability; production callers pass
    /// `Instant::now()`.
    pub fn push_edit_at(&mut self, state: T, now: Instant) {
        self.redo.clear();

        let coalesce = self
            .last_push
            .map(|t| now.duration_since(t) < COALESCE_WINDOW)
            .unwrap_or(false);

        if coalesce && !self.undo.is_empty() {
            *self.undo.last_mut().expect("non-empty") = state;
        } else {
            self.undo.push(state);
            if self.undo.len() > UNDO_STACK_CAP {
                self.undo.remove(0);
            }
        }
        self.last_push = Some(now);
    }

    /// Convenience wrapper over [`UndoStack::push_edit_at`] using
    /// `Instant::now()`.
    pub fn push_edit(&mut self, state: T) {
        self.push_edit_at(state, Instant::now());
    }

    /// Pop the most-recent undo snapshot. Caller supplies the *current* state
    /// to push onto the redo stack first (so redo can restore it). Returns the
    /// state the caller should load, or `None` if there is nothing to undo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        if self.redo.len() > UNDO_STACK_CAP {
            self.redo.remove(0);
        }
        self.last_push = None;
        Some(prev)
    }

    /// Inverse of [`UndoStack::undo`]. Caller supplies the current state; we
    /// push it onto the undo stack and return the most-recent redo snapshot.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        if self.undo.len() > UNDO_STACK_CAP {
            self.undo.remove(0);
        }
        self.last_push = None;
        Some(next)
    }

    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SlashAutocomplete
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in the autocomplete popup.
///
/// Owns its strings so user-defined skills (loaded at runtime from disk) can
/// participate in the popup alongside the static built-in registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// Command name (no leading `/`).
    pub name: String,
    /// One-line description shown after the name.
    pub description: String,
    /// Argument hint, e.g. `"claude | gpt | codex"`. Empty for zero-arg cmds.
    pub args_hint: String,
}

/// Slash-command completion state.
///
/// Construction triggers and updates are driven from the input buffer via
/// [`SlashAutocomplete::should_open`] (for `'/'` first char) and
/// [`SlashAutocomplete::refresh`] (after every buffer change). The popup stays
/// open as long as the buffer starts with `/` and has no space yet.
///
/// Built-ins always come first in the popup so they shadow same-named skills
/// (matching the slash dispatcher's resolution order); skills follow in the
/// order they were supplied by the caller (typically alphabetical, which is
/// how `SkillRegistry::names_with_descriptions` sorts them).
///
/// Navigation: [`SlashAutocomplete::next`] / [`SlashAutocomplete::prev`] move
/// the selection; [`SlashAutocomplete::accept`] returns the full replacement
/// text (`"/<name>"`) the caller should install in the buffer.
#[derive(Debug, Clone)]
pub struct SlashAutocomplete {
    items: Vec<AutocompleteItem>,
    selected: usize,
    /// Snapshot of the skill list this popup was built against. Held by
    /// value so [`Self::refresh`] doesn't need a re-borrow of the registry —
    /// the chat input refreshes on every keystroke and we want that loop to
    /// stay borrow-free against `&mut UiState`.
    skills: Vec<(String, String)>,
}

impl SlashAutocomplete {
    /// Returns `true` when the current `buffer` value looks like the user is
    /// starting a slash command: starts with `/`, and does *not* yet contain a
    /// space (which would indicate argument entry — see [`Self::refresh`]).
    pub fn should_open(buffer: &str) -> bool {
        buffer.starts_with('/') && !buffer.contains(' ')
    }

    /// Create a fresh popup for the given buffer using only built-in commands.
    /// Returns `None` when the buffer doesn't match [`Self::should_open`] or
    /// no commands match the prefix.
    ///
    /// Production callers should prefer [`Self::new_with_skills`]; this entry
    /// point exists for callers (and tests) that don't have a skill registry.
    #[allow(dead_code)] // exercised by editor tests; production calls go through new_with_skills
    pub fn new(buffer: &str) -> Option<Self> {
        Self::new_with_skills(buffer, Vec::new())
    }

    /// Like [`Self::new`] but also surfaces `skills` (`(name, description)`
    /// pairs) as completion candidates. Skills are filtered by the same prefix
    /// rule and rendered with a `[skill]` suffix on the description so users
    /// can see at a glance that a candidate isn't a built-in.
    pub fn new_with_skills(buffer: &str, skills: Vec<(String, String)>) -> Option<Self> {
        if !Self::should_open(buffer) {
            return None;
        }
        let items = filter_all(buffer, &skills);
        if items.is_empty() {
            return None;
        }
        Some(Self {
            items,
            selected: 0,
            skills,
        })
    }

    /// Re-filter against the current buffer. Returns `false` when the popup
    /// should close (buffer no longer qualifies, or no matches). The caller is
    /// responsible for dropping the `Option<SlashAutocomplete>` in that case.
    pub fn refresh(&mut self, buffer: &str) -> bool {
        if !Self::should_open(buffer) {
            return false;
        }
        let items = filter_all(buffer, &self.skills);
        if items.is_empty() {
            return false;
        }
        // Try to keep the selection on the same command if it still matches;
        // otherwise clamp to the front.
        let keep_name = self.items.get(self.selected).map(|i| i.name.clone());
        self.items = items;
        if let Some(name) = keep_name {
            if let Some(idx) = self.items.iter().position(|i| i.name == name) {
                self.selected = idx;
                return true;
            }
        }
        self.selected = 0;
        true
    }

    pub fn items(&self) -> &[AutocompleteItem] {
        &self.items
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    pub fn prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.items.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    /// Return the `/command` string the caller should install as the new
    /// buffer contents. `None` if the popup is empty.
    pub fn accept(&self) -> Option<String> {
        self.items
            .get(self.selected)
            .map(|i| format!("/{}", i.name))
    }
}

/// Case-insensitive prefix filter over the built-in registry. Preserves the
/// canonical registry order. Used as the first-pass filter in [`filter_all`];
/// kept separate so the built-in-only test path is trivially exercisable.
fn filter_commands(buffer: &str) -> Vec<AutocompleteItem> {
    // Strip leading `/` and lowercase for matching.
    let rest = buffer.strip_prefix('/').unwrap_or(buffer);
    let needle = rest.to_ascii_lowercase();
    BUILTIN_COMMANDS
        .iter()
        .filter(|e| e.name.to_ascii_lowercase().starts_with(&needle))
        .map(|e| AutocompleteItem {
            name: e.name.to_string(),
            description: e.description.to_string(),
            args_hint: e.args_hint.to_string(),
        })
        .collect()
}

/// Combined built-ins + user-skills filter. Built-ins always come first; a
/// skill whose name shadows a built-in is silently dropped from the popup
/// (same precedence as the slash dispatcher).
fn filter_all(buffer: &str, skills: &[(String, String)]) -> Vec<AutocompleteItem> {
    let mut items = filter_commands(buffer);
    let rest = buffer.strip_prefix('/').unwrap_or(buffer);
    let needle = rest.to_ascii_lowercase();
    let builtin_names: std::collections::HashSet<String> =
        items.iter().map(|i| i.name.clone()).collect();
    let builtin_set: std::collections::HashSet<&str> =
        BUILTIN_COMMANDS.iter().map(|e| e.name).collect();
    for (name, desc) in skills {
        if !name.to_ascii_lowercase().starts_with(&needle) {
            continue;
        }
        if builtin_set.contains(name.as_str()) || builtin_names.contains(name) {
            continue;
        }
        items.push(AutocompleteItem {
            name: name.clone(),
            description: format!("{desc}  [skill]"),
            args_hint: String::new(),
        });
    }
    items
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ---- KillRing ----------------------------------------------------------

    #[test]
    fn kill_yank_roundtrip() {
        let mut k = KillRing::new();
        k.kill("hello", false);
        assert_eq!(k.yank(), Some("hello"));
        assert_eq!(k.len(), 1);
    }

    #[test]
    fn empty_kill_is_noop() {
        let mut k = KillRing::new();
        k.kill("", false);
        assert_eq!(k.yank(), None);
        assert_eq!(k.len(), 0);
    }

    #[test]
    fn consecutive_kills_append_forward() {
        // Two Ctrl+K's in a row → single entry, concatenated in order.
        let mut k = KillRing::new();
        k.kill("foo", false);
        k.kill(" bar", false);
        assert_eq!(k.yank(), Some("foo bar"));
        assert_eq!(k.len(), 1);
    }

    #[test]
    fn consecutive_kills_prepend_backward() {
        // Two Ctrl+W's in a row → single entry, with later kill at the front.
        let mut k = KillRing::new();
        k.kill("world", true);
        k.kill("hello ", true);
        assert_eq!(k.yank(), Some("hello world"));
        assert_eq!(k.len(), 1);
    }

    #[test]
    fn boundary_starts_new_entry() {
        let mut k = KillRing::new();
        k.kill("first", false);
        k.mark_boundary();
        k.kill("second", false);
        assert_eq!(k.len(), 2);
        assert_eq!(k.yank(), Some("second"));
    }

    #[test]
    fn yank_pop_cycles_entries() {
        let mut k = KillRing::new();
        k.kill("a", false);
        k.mark_boundary();
        k.kill("b", false);
        k.mark_boundary();
        k.kill("c", false);
        // newest-first: c, b, a
        assert_eq!(k.yank(), Some("c"));
        // yank-pop: c moves to front, b surfaces
        assert_eq!(k.yank_pop(), Some("b"));
        // yank-pop again: b moves to front, a surfaces
        assert_eq!(k.yank_pop(), Some("a"));
        // and again: a moves to front, c surfaces
        assert_eq!(k.yank_pop(), Some("c"));
    }

    #[test]
    fn yank_pop_on_single_entry_is_stable() {
        let mut k = KillRing::new();
        k.kill("only", false);
        assert_eq!(k.yank_pop(), Some("only"));
        assert_eq!(k.yank_pop(), Some("only"));
    }

    #[test]
    fn ring_respects_cap() {
        let mut k = KillRing::new();
        for i in 0..KILL_RING_CAP + 10 {
            k.mark_boundary();
            k.kill(&format!("entry{i}"), false);
        }
        assert_eq!(k.len(), KILL_RING_CAP);
        // Newest still at head.
        assert_eq!(
            k.yank(),
            Some(format!("entry{}", KILL_RING_CAP + 9).as_str())
        );
    }

    // ---- UndoStack ---------------------------------------------------------

    #[test]
    fn undo_redo_flow() {
        let mut s: UndoStack<String> = UndoStack::new();
        s.push("a".into());
        s.push("ab".into());
        s.push("abc".into());
        // current buffer is "abcd"; undo three times
        let u1 = s.undo("abcd".into()).expect("undo1");
        assert_eq!(u1, "abc");
        let u2 = s.undo(u1).expect("undo2");
        assert_eq!(u2, "ab");
        let u3 = s.undo(u2).expect("undo3");
        assert_eq!(u3, "a");
        assert!(s.undo(u3.clone()).is_none());

        // Redo path now reverses. The redo stack, top-down, holds the
        // snapshots we replaced during undo in reverse order: "ab", "abc",
        // "abcd". So the first redo pulls "ab" back.
        let r1 = s.redo(u3).expect("redo1");
        assert_eq!(r1, "ab");
        let r2 = s.redo(r1).expect("redo2");
        assert_eq!(r2, "abc");
        let r3 = s.redo(r2).expect("redo3");
        assert_eq!(r3, "abcd");
        assert!(s.redo(r3.clone()).is_none());
    }

    #[test]
    fn coalesce_merges_rapid_edits() {
        let mut s: UndoStack<String> = UndoStack::new();
        let t0 = Instant::now();
        s.push_edit_at("a".into(), t0);
        s.push_edit_at("ab".into(), t0 + Duration::from_millis(100));
        s.push_edit_at("abc".into(), t0 + Duration::from_millis(200));
        // All within the 500 ms window → exactly one entry.
        assert!(s.can_undo());
        let u = s.undo("abcd".into()).expect("undo");
        assert_eq!(u, "abc", "should restore the coalesced top");
        assert!(!s.can_undo(), "only one entry was pushed under coalesce");
    }

    #[test]
    fn coalesce_breaks_after_window() {
        let mut s: UndoStack<String> = UndoStack::new();
        let t0 = Instant::now();
        s.push_edit_at("a".into(), t0);
        s.push_edit_at("ab".into(), t0 + Duration::from_millis(600)); // past 500 ms
                                                                      // Two distinct entries expected.
        let u1 = s.undo("abc".into()).expect("undo1");
        assert_eq!(u1, "ab");
        let u2 = s.undo(u1).expect("undo2");
        assert_eq!(u2, "a");
    }

    #[test]
    fn hard_push_does_not_coalesce() {
        let mut s: UndoStack<String> = UndoStack::new();
        let t0 = Instant::now();
        s.push_edit_at("a".into(), t0);
        s.push("ab".into()); // hard boundary
        s.push_edit_at("abc".into(), t0 + Duration::from_millis(50));
        // push() after push_edit resets last_push, so the next push_edit is
        // also a fresh entry.
        assert!(s.can_undo());
        let u1 = s.undo("abcd".into()).expect("undo1");
        assert_eq!(u1, "abc");
        let u2 = s.undo(u1).expect("undo2");
        assert_eq!(u2, "ab");
        let u3 = s.undo(u2).expect("undo3");
        assert_eq!(u3, "a");
    }

    #[test]
    fn new_push_clears_redo() {
        let mut s: UndoStack<String> = UndoStack::new();
        s.push("a".into());
        s.push("ab".into());
        let _ = s.undo("abc".into());
        assert!(s.can_redo());
        s.push("aX".into());
        assert!(!s.can_redo(), "new push must clear redo");
    }

    // ---- SlashAutocomplete -------------------------------------------------

    #[test]
    fn should_open_rules() {
        assert!(SlashAutocomplete::should_open("/"));
        assert!(SlashAutocomplete::should_open("/he"));
        assert!(SlashAutocomplete::should_open("/handoff"));
        // A space terminates the name portion.
        assert!(!SlashAutocomplete::should_open("/handoff "));
        assert!(!SlashAutocomplete::should_open("/handoff gpt"));
        // No leading slash → no popup.
        assert!(!SlashAutocomplete::should_open(""));
        assert!(!SlashAutocomplete::should_open("help"));
    }

    #[test]
    fn filters_by_prefix() {
        let ac = SlashAutocomplete::new("/h").expect("popup");
        let names: Vec<&str> = ac.items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"help"));
        assert!(names.contains(&"hotkeys"));
        assert!(names.contains(&"handoff"));
        // Does not contain unrelated commands.
        assert!(!names.contains(&"quit"));
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let ac = SlashAutocomplete::new("/HE").expect("popup");
        let names: Vec<&str> = ac.items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"help"));
    }

    #[test]
    fn returns_none_when_no_match() {
        assert!(SlashAutocomplete::new("/xyznomatch").is_none());
    }

    #[test]
    fn refresh_closes_on_space() {
        let mut ac = SlashAutocomplete::new("/h").expect("popup");
        assert!(!ac.refresh("/handoff "));
    }

    #[test]
    fn navigation_wraps() {
        let mut ac = SlashAutocomplete::new("/h").expect("popup");
        let n = ac.items().len();
        assert!(n >= 2, "need ≥2 items to test wrap");
        let start = ac.selected_index();
        for _ in 0..n {
            ac.next();
        }
        assert_eq!(ac.selected_index(), start, "wrap after n next()s");

        ac.prev();
        assert_eq!(ac.selected_index(), (start + n - 1) % n);
    }

    #[test]
    fn accept_returns_slash_name() {
        let mut ac = SlashAutocomplete::new("/h").expect("popup");
        // Move to 'handoff' (second of the h-prefixed commands alphabetically
        // in the registry: help, hotkeys, handoff). Rather than hardcode an
        // index, find by name.
        let idx = ac
            .items()
            .iter()
            .position(|i| i.name == "handoff")
            .expect("handoff matches /h");
        // Rotate selection to that index.
        while ac.selected_index() != idx {
            ac.next();
        }
        assert_eq!(ac.accept(), Some("/handoff".to_string()));
    }

    #[test]
    fn refresh_keeps_selection_on_same_name() {
        let mut ac = SlashAutocomplete::new("/h").expect("popup");
        // Pin selection to 'help' (index 0 currently in builtins).
        let help_idx = ac
            .items()
            .iter()
            .position(|i| i.name == "help")
            .expect("help in /h results");
        while ac.selected_index() != help_idx {
            ac.next();
        }
        assert!(ac.refresh("/hel"));
        assert_eq!(ac.items().len(), 1);
        assert_eq!(ac.items()[0].name, "help");
        assert_eq!(ac.selected_index(), 0);
        assert_eq!(ac.accept(), Some("/help".to_string()));
    }

    #[test]
    fn tab_acceptance_workflow() {
        // Simulates the Tab-to-accept flow:
        //   1. user types '/', popup opens
        //   2. user types 'ho', popup refreshes
        //   3. user hits Tab → we accept; buffer becomes '/hotkeys'
        let mut ac = SlashAutocomplete::new("/").expect("popup");
        assert!(ac.refresh("/ho"));
        // Must contain 'hotkeys' at minimum (from registry order: help,
        // hotkeys, ...).
        let idx = ac
            .items()
            .iter()
            .position(|i| i.name == "hotkeys")
            .expect("hotkeys matches /ho");
        while ac.selected_index() != idx {
            ac.next();
        }
        assert_eq!(ac.accept(), Some("/hotkeys".to_string()));
    }
}

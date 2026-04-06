use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

/// Actions the TUI can perform in response to input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    ToggleHelp,
    SwitchScreen(usize), // 0-indexed
    NextPane,
    PrevPane,
    MoveUp,
    MoveDown,
    Top,
    Bottom,
    Select,
    Back,
    Refresh,
    ToggleLogSource,
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    ConfirmFilter,
    Tick,
    None,
}

/// Poll for input events with a given timeout.
pub fn poll_event(timeout: Duration) -> Action {
    if event::poll(timeout).unwrap_or(false) {
        if let Ok(Event::Key(key)) = event::read() {
            return map_key(key);
        }
    }
    Action::Tick
}

fn map_key(key: KeyEvent) -> Action {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('1') => Action::SwitchScreen(0),
        KeyCode::Char('2') => Action::SwitchScreen(1),
        KeyCode::Char('3') => Action::SwitchScreen(2),
        KeyCode::Char('4') => Action::SwitchScreen(3),
        KeyCode::Char('5') => Action::SwitchScreen(4),
        KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
        KeyCode::Char('g') => Action::Top,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('t') => Action::ToggleLogSource,
        KeyCode::Char('/') => Action::StartFilter,
        KeyCode::Enter => Action::Select,
        KeyCode::Esc => Action::Back,
        KeyCode::Tab => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::PrevPane
            } else {
                Action::NextPane
            }
        }
        KeyCode::BackTab => Action::PrevPane,
        _ => Action::None,
    }
}

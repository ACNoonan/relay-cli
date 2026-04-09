use super::events::Action;
use super::state::*;

/// Process an action and mutate app state. Returns true if a data refresh is needed.
pub fn handle_action(state: &mut AppState, action: Action) -> bool {
    let mut needs_refresh = false;

    match action {
        Action::Quit => {
            if state.filter_active {
                state.filter_active = false;
                state.filter_text.clear();
            } else if state.show_help {
                state.show_help = false;
            } else {
                state.running = false;
            }
        }
        Action::ToggleHelp => {
            state.show_help = !state.show_help;
        }
        Action::SwitchScreen(idx) => {
            if let Some(&screen) = Screen::ALL.get(idx) {
                state.screen = screen;
                state.focus = Pane::Left;
                state.show_help = false;
                state.filter_active = false;
                state.filter_text.clear();
                needs_refresh = true;
            }
        }
        Action::NextPane => {
            state.focus = match state.focus {
                Pane::Left => Pane::Right,
                Pane::Right => Pane::Left,
            };
        }
        Action::PrevPane => {
            state.focus = match state.focus {
                Pane::Left => Pane::Right,
                Pane::Right => Pane::Left,
            };
        }
        Action::MoveUp => {
            if state.filter_active {
                return false;
            }
            let idx = state.current_index();
            if idx > 0 {
                state.set_current_index(idx - 1);
            }
        }
        Action::MoveDown => {
            if state.filter_active {
                return false;
            }
            let idx = state.current_index();
            let len = state.current_list_len();
            if len > 0 && idx < len - 1 {
                state.set_current_index(idx + 1);
            }
        }
        Action::Top => {
            if !state.filter_active {
                state.set_current_index(0);
            }
        }
        Action::Bottom => {
            if !state.filter_active {
                let len = state.current_list_len();
                if len > 0 {
                    state.set_current_index(len - 1);
                }
            }
        }
        Action::Select => {
            if state.filter_active {
                state.filter_active = false;
                return false;
            }
            // On sessions screen, pressing Enter could switch to detail pane
            if state.screen == Screen::Sessions && state.focus == Pane::Left {
                state.focus = Pane::Right;
            }
            // On logs screen left pane, select session and move to right pane
            if state.screen == Screen::Logs && state.focus == Pane::Left {
                state.focus = Pane::Right;
                state.log_scroll = 0;
                needs_refresh = true;
            }
            // On reviews screen, pressing Enter switches to detail pane
            if state.screen == Screen::Reviews && state.focus == Pane::Left {
                state.focus = Pane::Right;
            }
        }
        Action::Back => {
            if state.filter_active {
                state.filter_active = false;
                state.filter_text.clear();
            } else if state.show_help {
                state.show_help = false;
            } else if state.focus == Pane::Right {
                state.focus = Pane::Left;
            }
        }
        Action::Refresh => {
            state.status_message = Some("REFRESHING...".to_string());
            needs_refresh = true;
        }
        Action::ToggleLogSource => {
            if state.screen == Screen::Logs {
                state.log_source = match state.log_source {
                    LogSource::Stdout => LogSource::Stderr,
                    LogSource::Stderr => LogSource::Stdout,
                };
                state.log_scroll = 0;
                needs_refresh = true;
            }
        }
        Action::StartFilter => {
            state.filter_active = true;
            state.filter_text.clear();
        }
        Action::FilterChar(c) => {
            if state.filter_active {
                state.filter_text.push(c);
            }
        }
        Action::FilterBackspace => {
            if state.filter_active {
                state.filter_text.pop();
            }
        }
        Action::Tick | Action::None => {}
    }

    needs_refresh
}

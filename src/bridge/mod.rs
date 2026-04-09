mod claude_backend;
mod openai_client;

use anyhow::{Context, Result};
use claude_backend::{ClaudeBackend, SubprocessBackend};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use openai_client::OpenAiClient;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{io, time::Duration};
use tokio::sync::mpsc;

const DEFAULT_REVIEWER_PROMPT: &str = "You are a senior engineering reviewer. \
Review the implementation from Claude critically for correctness, edge cases, \
security, and architecture. Use severity levels CRITICAL / CONCERN / SUGGESTION. \
Conclude with APPROVED when there are no blocking issues.";

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub prompt: String,
    pub claude_model: Option<String>,
    pub claude_binary: String,
    pub gpt_model: String,
    pub reviewer_prompt_file: Option<String>,
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptTarget {
    Claude,
    Gpt,
}

impl PromptTarget {
    fn toggle(&mut self) {
        *self = match self {
            PromptTarget::Claude => PromptTarget::Gpt,
            PromptTarget::Gpt => PromptTarget::Claude,
        };
    }

    fn label(self) -> &'static str {
        match self {
            PromptTarget::Claude => "Claude",
            PromptTarget::Gpt => "GPT",
        }
    }
}

impl Default for PromptTarget {
    fn default() -> Self {
        Self::Claude
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivePane {
    Claude,
    Gpt,
}

impl ActivePane {
    fn toggle(&mut self) {
        *self = match self {
            ActivePane::Claude => ActivePane::Gpt,
            ActivePane::Gpt => ActivePane::Claude,
        };
    }
}

impl Default for ActivePane {
    fn default() -> Self {
        Self::Claude
    }
}

#[derive(Debug, Clone)]
enum BridgeCommand {
    SubmitPrompt {
        target: PromptTarget,
        prompt: String,
        auto_route_after_claude: bool,
    },
    RouteFromPane(ActivePane),
    RerunLast,
    NewSession,
    Quit,
}

#[derive(Debug)]
enum BridgeEvent {
    Status(String),
    SetRunning(bool),
    ReplaceClaude(String),
    ReplaceGpt(String),
    ClaudeDelta(String),
    SetSessionId(Option<String>),
    GptDelta(String),
    Error(String),
}

#[derive(Default)]
struct BridgeState {
    claude_text: String,
    gpt_text: String,
    session_id: Option<String>,
    status: String,
    running_job: bool,
    error: Option<String>,
    input: String,
    prompt_target: PromptTarget,
    active_pane: ActivePane,
    claude_scroll: usize,
    gpt_scroll: usize,
    search_mode: bool,
    search_query: String,
    search_match_index: usize,
}

impl BridgeState {
    fn active_pane_text(&self) -> &str {
        match self.active_pane {
            ActivePane::Claude => &self.claude_text,
            ActivePane::Gpt => &self.gpt_text,
        }
    }

    fn active_scroll_mut(&mut self) -> &mut usize {
        match self.active_pane {
            ActivePane::Claude => &mut self.claude_scroll,
            ActivePane::Gpt => &mut self.gpt_scroll,
        }
    }

    fn clamp_scrolls(&mut self, viewport_height: usize) {
        self.claude_scroll = self
            .claude_scroll
            .min(max_scroll_for_text(&self.claude_text, viewport_height));
        self.gpt_scroll = self
            .gpt_scroll
            .min(max_scroll_for_text(&self.gpt_text, viewport_height));
    }
}

pub async fn run(options: BridgeOptions) -> Result<()> {
    let reviewer_prompt = load_reviewer_prompt(options.reviewer_prompt_file.as_deref())?;
    let (event_tx, mut event_rx) = mpsc::channel::<BridgeEvent>(2048);
    let (cmd_tx, cmd_rx) = mpsc::channel::<BridgeCommand>(64);
    let worker_options = options.clone();
    let initial_prompt = options.prompt.clone();

    tokio::spawn(async move {
        if let Err(err) =
            run_worker_loop(worker_options, reviewer_prompt, cmd_rx, event_tx.clone()).await
        {
            let _ = event_tx.send(BridgeEvent::Error(err.to_string())).await;
        }
    });

    cmd_tx
        .send(BridgeCommand::SubmitPrompt {
            target: PromptTarget::Claude,
            prompt: initial_prompt,
            auto_route_after_claude: true,
        })
        .await
        .ok();

    run_tui(&mut event_rx, &cmd_tx)?;
    Ok(())
}

async fn run_worker_loop(
    options: BridgeOptions,
    reviewer_prompt: String,
    mut cmd_rx: mpsc::Receiver<BridgeCommand>,
    tx: mpsc::Sender<BridgeEvent>,
) -> Result<()> {
    let claude = SubprocessBackend::new(options.claude_binary.clone());
    let openai = OpenAiClient::from_env()?;

    let mut worker_state = WorkerState {
        session_id: options.resume_session_id.clone(),
        last_claude_output: String::new(),
        last_gpt_output: String::new(),
        last_command: None,
    };

    tx.send(BridgeEvent::SetSessionId(worker_state.session_id.clone()))
        .await
        .ok();
    tx.send(BridgeEvent::Status(
        "Bridge ready. Enter prompt and press Enter.".to_string(),
    ))
    .await
    .ok();

    while let Some(command) = cmd_rx.recv().await {
        if let BridgeCommand::Quit = command {
            break;
        }

        let run_result = execute_command(
            command.clone(),
            &claude,
            &openai,
            &options,
            &reviewer_prompt,
            &tx,
            &mut worker_state,
        )
        .await;

        if let Err(err) = run_result {
            tx.send(BridgeEvent::Error(err.to_string())).await.ok();
        }
    }

    Ok(())
}

struct WorkerState {
    session_id: Option<String>,
    last_claude_output: String,
    last_gpt_output: String,
    last_command: Option<BridgeCommand>,
}

async fn execute_command(
    command: BridgeCommand,
    claude: &SubprocessBackend,
    openai: &OpenAiClient,
    options: &BridgeOptions,
    reviewer_prompt: &str,
    tx: &mpsc::Sender<BridgeEvent>,
    state: &mut WorkerState,
) -> Result<()> {
    tx.send(BridgeEvent::SetRunning(true)).await.ok();
    let mut update_last_command = true;
    let effective_command = if let BridgeCommand::RerunLast = command {
        update_last_command = false;
        if let Some(last) = state.last_command.clone() {
            tx.send(BridgeEvent::Status(
                "Rerunning last operation...".to_string(),
            ))
            .await
            .ok();
            last
        } else {
            tx.send(BridgeEvent::Status(
                "No previous operation to rerun.".to_string(),
            ))
            .await
            .ok();
            tx.send(BridgeEvent::SetRunning(false)).await.ok();
            return Ok(());
        }
    } else {
        command.clone()
    };

    match &effective_command {
        BridgeCommand::SubmitPrompt {
            target,
            prompt,
            auto_route_after_claude,
        } => match target {
            PromptTarget::Claude => {
                let resume = state.session_id.clone();
                run_claude_prompt(
                    claude,
                    options.claude_model.as_deref(),
                    prompt,
                    resume.as_deref(),
                    tx,
                    &mut state.last_claude_output,
                    &mut state.session_id,
                )
                .await?;

                if *auto_route_after_claude {
                    run_gpt_review(
                        openai,
                        &options.gpt_model,
                        reviewer_prompt,
                        &state.last_claude_output,
                        tx,
                        &mut state.last_gpt_output,
                    )
                    .await?;
                }
            }
            PromptTarget::Gpt => {
                run_gpt_prompt(
                    openai,
                    &options.gpt_model,
                    reviewer_prompt,
                    prompt,
                    tx,
                    &mut state.last_gpt_output,
                )
                .await?;
            }
        },
        BridgeCommand::RouteFromPane(source) => match source {
            ActivePane::Claude => {
                if state.last_claude_output.trim().is_empty() {
                    tx.send(BridgeEvent::Status(
                        "No Claude output to route.".to_string(),
                    ))
                    .await
                    .ok();
                } else {
                    run_gpt_review(
                        openai,
                        &options.gpt_model,
                        reviewer_prompt,
                        &state.last_claude_output,
                        tx,
                        &mut state.last_gpt_output,
                    )
                    .await?;
                }
            }
            ActivePane::Gpt => {
                if state.last_gpt_output.trim().is_empty() {
                    tx.send(BridgeEvent::Status("No GPT output to route.".to_string()))
                        .await
                        .ok();
                } else {
                    let followup = format!(
                        "Apply this reviewer feedback to the current implementation:\n\n{}",
                        state.last_gpt_output
                    );
                    let resume = state.session_id.clone();
                    run_claude_prompt(
                        claude,
                        options.claude_model.as_deref(),
                        &followup,
                        resume.as_deref(),
                        tx,
                        &mut state.last_claude_output,
                        &mut state.session_id,
                    )
                    .await?;
                }
            }
        },
        BridgeCommand::RerunLast => {}
        BridgeCommand::NewSession => {
            state.session_id = None;
            tx.send(BridgeEvent::SetSessionId(None)).await.ok();
            tx.send(BridgeEvent::Status(
                "Started new Claude session context.".to_string(),
            ))
            .await
            .ok();
        }
        BridgeCommand::Quit => {}
    }

    if update_last_command && !matches!(effective_command, BridgeCommand::Quit) {
        state.last_command = Some(effective_command);
    }

    tx.send(BridgeEvent::SetRunning(false)).await.ok();
    Ok(())
}

async fn run_claude_prompt(
    claude: &SubprocessBackend,
    model: Option<&str>,
    prompt: &str,
    resume_session_id: Option<&str>,
    tx: &mpsc::Sender<BridgeEvent>,
    claude_buffer: &mut String,
    session_id_out: &mut Option<String>,
) -> Result<()> {
    tx.send(BridgeEvent::Status(
        "Streaming Claude output...".to_string(),
    ))
    .await
    .ok();
    tx.send(BridgeEvent::ReplaceClaude(String::new()))
        .await
        .ok();

    let delta_tx = tx.clone();
    let mut on_claude_delta = move |chunk: String| {
        let _ = delta_tx.blocking_send(BridgeEvent::ClaudeDelta(chunk));
    };

    let turn = claude
        .run_prompt_stream(prompt, model, resume_session_id, &mut on_claude_delta)
        .await?;

    *claude_buffer = turn.full_text;
    *session_id_out = turn.session_id;
    tx.send(BridgeEvent::SetSessionId(session_id_out.clone()))
        .await
        .ok();
    Ok(())
}

async fn run_gpt_review(
    openai: &OpenAiClient,
    model: &str,
    reviewer_prompt: &str,
    claude_output: &str,
    tx: &mpsc::Sender<BridgeEvent>,
    gpt_buffer: &mut String,
) -> Result<()> {
    tx.send(BridgeEvent::Status(
        "Streaming GPT verification...".to_string(),
    ))
    .await
    .ok();
    tx.send(BridgeEvent::ReplaceGpt(String::new())).await.ok();

    let delta_tx = tx.clone();
    let mut on_gpt_delta = move |chunk: String| {
        let _ = delta_tx.blocking_send(BridgeEvent::GptDelta(chunk));
    };
    let gpt_input = format!(
        "Review this Claude output:\n\n---\n{}\n---\n\nProvide a critical engineering review.",
        claude_output
    );

    let full = openai
        .stream_chat_completion(model, reviewer_prompt, &gpt_input, &mut on_gpt_delta)
        .await?;
    *gpt_buffer = full;
    tx.send(BridgeEvent::Status(
        "Verification complete. Enter next prompt.".to_string(),
    ))
    .await
    .ok();
    Ok(())
}

async fn run_gpt_prompt(
    openai: &OpenAiClient,
    model: &str,
    reviewer_prompt: &str,
    prompt: &str,
    tx: &mpsc::Sender<BridgeEvent>,
    gpt_buffer: &mut String,
) -> Result<()> {
    tx.send(BridgeEvent::Status("Streaming GPT response...".to_string()))
        .await
        .ok();
    tx.send(BridgeEvent::ReplaceGpt(String::new())).await.ok();

    let delta_tx = tx.clone();
    let mut on_gpt_delta = move |chunk: String| {
        let _ = delta_tx.blocking_send(BridgeEvent::GptDelta(chunk));
    };

    let full = openai
        .stream_chat_completion(model, reviewer_prompt, prompt, &mut on_gpt_delta)
        .await?;
    *gpt_buffer = full;
    tx.send(BridgeEvent::Status(
        "GPT turn complete. Enter next prompt.".to_string(),
    ))
    .await
    .ok();
    Ok(())
}

fn run_tui(
    rx: &mut mpsc::Receiver<BridgeEvent>,
    cmd_tx: &mpsc::Sender<BridgeCommand>,
) -> Result<()> {
    enable_raw_mode().context("enabling raw mode for bridge TUI")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating bridge terminal")?;

    let mut state = BridgeState {
        status: "Starting bridge workflow...".to_string(),
        prompt_target: PromptTarget::Claude,
        active_pane: ActivePane::Claude,
        ..BridgeState::default()
    };

    loop {
        drain_events(rx, &mut state);

        terminal
            .draw(|f| render(f, &state))
            .context("drawing bridge UI")?;

        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    let _ = cmd_tx.blocking_send(BridgeCommand::Quit);
                    break;
                }

                if key.code == KeyCode::Tab {
                    state.active_pane.toggle();
                    continue;
                }

                if state.search_mode {
                    match key.code {
                        KeyCode::Enter => {
                            apply_search(&mut state);
                            state.search_mode = false;
                        }
                        KeyCode::Esc => {
                            state.search_mode = false;
                        }
                        KeyCode::Backspace => {
                            state.search_query.pop();
                        }
                        KeyCode::Char(c) => {
                            state.search_query.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('r') => {
                            let _ = cmd_tx
                                .blocking_send(BridgeCommand::RouteFromPane(state.active_pane));
                        }
                        KeyCode::Char('t') => state.prompt_target.toggle(),
                        KeyCode::Char('f') => {
                            state.search_mode = true;
                        }
                        KeyCode::Char('n') => {
                            let _ = cmd_tx.blocking_send(BridgeCommand::NewSession);
                        }
                        KeyCode::Char('e') => {
                            let _ = cmd_tx.blocking_send(BridgeCommand::RerunLast);
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Up => {
                        let scroll = state.active_scroll_mut();
                        *scroll = scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        let scroll = state.active_scroll_mut();
                        *scroll = scroll.saturating_add(1);
                    }
                    KeyCode::PageUp => {
                        let scroll = state.active_scroll_mut();
                        *scroll = scroll.saturating_sub(10);
                    }
                    KeyCode::PageDown => {
                        let scroll = state.active_scroll_mut();
                        *scroll = scroll.saturating_add(10);
                    }
                    KeyCode::Home => {
                        *state.active_scroll_mut() = 0;
                    }
                    KeyCode::End => {
                        *state.active_scroll_mut() = usize::MAX / 2;
                    }
                    KeyCode::Char('n') => {
                        next_search_match(&mut state);
                    }
                    KeyCode::Char('N') => {
                        prev_search_match(&mut state);
                    }
                    KeyCode::Enter => {
                        if state.running_job {
                            state.status = "Bridge busy. Wait for current operation.".to_string();
                        } else if !state.input.trim().is_empty() {
                            let prompt = std::mem::take(&mut state.input);
                            let _ = cmd_tx.blocking_send(BridgeCommand::SubmitPrompt {
                                target: state.prompt_target,
                                prompt,
                                auto_route_after_claude: state.prompt_target
                                    == PromptTarget::Claude,
                            });
                        }
                    }
                    KeyCode::Backspace => {
                        state.input.pop();
                    }
                    KeyCode::Esc => {
                        state.input.clear();
                    }
                    KeyCode::Char(c) => {
                        state.input.push(c);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode().context("disabling raw mode for bridge TUI")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("leaving alternate screen")?;
    terminal.show_cursor().context("showing cursor")?;
    Ok(())
}

fn drain_events(rx: &mut mpsc::Receiver<BridgeEvent>, state: &mut BridgeState) {
    loop {
        match rx.try_recv() {
            Ok(BridgeEvent::Status(s)) => state.status = s,
            Ok(BridgeEvent::SetRunning(v)) => state.running_job = v,
            Ok(BridgeEvent::ReplaceClaude(s)) => {
                state.claude_text = s;
                state.claude_scroll = 0;
            }
            Ok(BridgeEvent::ReplaceGpt(s)) => {
                state.gpt_text = s;
                state.gpt_scroll = 0;
            }
            Ok(BridgeEvent::ClaudeDelta(chunk)) => {
                state.claude_text.push_str(&chunk);
                state.claude_scroll = usize::MAX / 2;
            }
            Ok(BridgeEvent::SetSessionId(session_id)) => state.session_id = session_id,
            Ok(BridgeEvent::GptDelta(chunk)) => {
                state.gpt_text.push_str(&chunk);
                state.gpt_scroll = usize::MAX / 2;
            }
            Ok(BridgeEvent::Error(err)) => {
                state.error = Some(err.clone());
                state.status = format!("Error: {} (q to quit)", err);
                state.running_job = false;
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

fn render(frame: &mut Frame, state: &BridgeState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let header = Paragraph::new("Relay Bridge  |  Live Claude <-> GPT verification").style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(header, layout[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(layout[1]);

    let mut render_state = BridgeState {
        ..BridgeState {
            claude_text: state.claude_text.clone(),
            gpt_text: state.gpt_text.clone(),
            session_id: state.session_id.clone(),
            status: state.status.clone(),
            running_job: state.running_job,
            error: state.error.clone(),
            input: state.input.clone(),
            prompt_target: state.prompt_target,
            active_pane: state.active_pane,
            claude_scroll: state.claude_scroll,
            gpt_scroll: state.gpt_scroll,
            search_mode: state.search_mode,
            search_query: state.search_query.clone(),
            search_match_index: state.search_match_index,
        }
    };
    let pane_height = panes[0].height.saturating_sub(2) as usize;
    render_state.clamp_scrolls(pane_height.max(1));

    render_output_pane(
        frame,
        panes[0],
        "Claude Output",
        &render_state.claude_text,
        render_state.claude_scroll,
        render_state.active_pane == ActivePane::Claude,
    );
    render_output_pane(
        frame,
        panes[1],
        "GPT Review",
        &render_state.gpt_text,
        render_state.gpt_scroll,
        render_state.active_pane == ActivePane::Gpt,
    );

    let search_hint = if render_state.search_mode {
        format!(" | Search: {}", render_state.search_query)
    } else {
        String::new()
    };
    let input_title = format!(
        "Prompt [{}] (Enter=send, Ctrl+T=target, Ctrl+F=search, Esc=clear{})",
        render_state.prompt_target.label(),
        search_hint
    );
    let input = Paragraph::new(render_state.input.clone())
        .block(Block::default().title(input_title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, layout[2]);

    let mut status = render_state.status.clone();
    if let Some(id) = &render_state.session_id {
        status.push_str(&format!(" | Claude session: {}", id));
    }
    if render_state.running_job {
        status.push_str(" | running");
    }
    if let Some(err) = &render_state.error {
        status = format!("{} | {}", status, err);
    }
    status.push_str(" | Up/Down/PgUp/PgDn scroll | n/N next/prev match | Tab pane");
    let footer = Paragraph::new(status)
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, layout[3]);
}

fn render_output_pane(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: &str,
    scroll: usize,
    active: bool,
) {
    let border_style = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let block = Block::default()
        .title(title)
        .border_style(border_style)
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_lines = inner.height.saturating_sub(1) as usize;
    let body = lines_for_viewport(text, scroll, max_lines.max(1));
    let paragraph = Paragraph::new(body).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn lines_for_viewport(text: &str, scroll: usize, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    if lines.len() <= max_lines && scroll == 0 {
        return text.to_string();
    }

    let max_scroll = lines.len().saturating_sub(max_lines);
    let clamped = scroll.min(max_scroll);
    let end = (clamped + max_lines).min(lines.len());
    lines[clamped..end].join("\n")
}

fn max_scroll_for_text(text: &str, max_lines: usize) -> usize {
    if max_lines == 0 {
        return 0;
    }
    let lines = text.lines().count();
    lines.saturating_sub(max_lines)
}

fn search_matches(text: &str, query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    text.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if line.to_lowercase().contains(&needle) {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

fn apply_search(state: &mut BridgeState) {
    let matches = search_matches(state.active_pane_text(), &state.search_query);
    if matches.is_empty() {
        state.status = "No search matches found in active pane.".to_string();
        state.search_match_index = 0;
        return;
    }
    state.search_match_index = 0;
    *state.active_scroll_mut() = matches[0];
    state.status = format!("Search match 1/{}", matches.len());
}

fn next_search_match(state: &mut BridgeState) {
    let matches = search_matches(state.active_pane_text(), &state.search_query);
    if matches.is_empty() {
        return;
    }
    state.search_match_index = (state.search_match_index + 1) % matches.len();
    *state.active_scroll_mut() = matches[state.search_match_index];
    state.status = format!(
        "Search match {}/{}",
        state.search_match_index + 1,
        matches.len()
    );
}

fn prev_search_match(state: &mut BridgeState) {
    let matches = search_matches(state.active_pane_text(), &state.search_query);
    if matches.is_empty() {
        return;
    }
    if state.search_match_index == 0 {
        state.search_match_index = matches.len() - 1;
    } else {
        state.search_match_index -= 1;
    }
    *state.active_scroll_mut() = matches[state.search_match_index];
    state.status = format!(
        "Search match {}/{}",
        state.search_match_index + 1,
        matches.len()
    );
}

fn load_reviewer_prompt(path: Option<&str>) -> Result<String> {
    if let Some(path) = path {
        return std::fs::read_to_string(path)
            .with_context(|| format!("reading reviewer prompt file at {path}"));
    }

    let default_path = std::path::Path::new("prompts/reviewer.txt");
    if default_path.is_file() {
        return std::fs::read_to_string(default_path).context("reading prompts/reviewer.txt");
    }

    Ok(DEFAULT_REVIEWER_PROMPT.to_string())
}

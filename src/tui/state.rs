use chrono::{DateTime, Utc};
use uuid::Uuid;

// ── Screen & Focus ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Sessions,
    Logs,
    Artifacts,
    Reviews,
}

impl Screen {
    pub const ALL: [Screen; 5] = [
        Screen::Overview,
        Screen::Sessions,
        Screen::Logs,
        Screen::Artifacts,
        Screen::Reviews,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Screen::Overview => "OVERVIEW",
            Screen::Sessions => "SESSIONS",
            Screen::Logs => "LOGS",
            Screen::Artifacts => "ARTIFACTS",
            Screen::Reviews => "REVIEWS",
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Screen::Overview => "1",
            Screen::Sessions => "2",
            Screen::Logs => "3",
            Screen::Artifacts => "4",
            Screen::Reviews => "5",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Left,
    Right,
}

// ── View-Model Snapshots ───────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct OverviewSnapshot {
    pub harness_initialized: bool,
    pub git_repo: bool,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub provider_checks: Vec<ProviderCheckRow>,
    pub session_counts: SessionCounts,
    pub recent_sessions: Vec<SessionRow>,
    pub recent_artifacts: Vec<ArtifactRow>,
    pub recent_reviews: Vec<ReviewRow>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionCounts {
    pub running: usize,
    pub completed: usize,
    pub crashed: usize,
    pub stopped: usize,
}

impl SessionCounts {
    pub fn total(&self) -> usize {
        self.running + self.completed + self.crashed + self.stopped
    }
}

#[derive(Debug, Clone)]
pub struct ProviderCheckRow {
    pub name: String,
    pub installed: bool,
    pub auth: bool,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: Uuid,
    pub short_id: String,
    pub provider: String,
    pub role: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SessionDetail {
    pub id: Uuid,
    pub provider: String,
    pub role: String,
    pub status: String,
    pub model: Option<String>,
    pub cwd: String,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub artifact_dir: String,
    pub log_dir: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub id: Uuid,
    pub short_id: String,
    pub artifact_type: String,
    pub created_at: DateTime<Utc>,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub short_id: String,
    pub provider: String,
    pub verdict: String,
    pub created_at: DateTime<Utc>,
    pub goal: String,
    pub finding_count: usize,
}

#[derive(Debug, Clone)]
pub struct ReviewDetail {
    pub provider: String,
    pub model: Option<String>,
    pub verdict: String,
    pub created_at: DateTime<Utc>,
    pub summary: String,
    pub findings: Vec<FindingRow>,
}

#[derive(Debug, Clone)]
pub struct FindingRow {
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
}

// ── Log Buffer ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogSource {
    #[default]
    Stdout,
    Stderr,
}

impl LogSource {
    pub fn label(&self) -> &'static str {
        match self {
            LogSource::Stdout => "STDOUT",
            LogSource::Stderr => "STDERR",
        }
    }
}

// ── Data Snapshot (all loaded data) ────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct DataSnapshot {
    pub overview: OverviewSnapshot,
    pub sessions: Vec<SessionRow>,
    pub session_details: Vec<SessionDetail>,
    pub artifacts: Vec<ArtifactRow>,
    pub reviews: Vec<ReviewRow>,
    pub review_details: Vec<ReviewDetail>,
    pub log_buffer: LogBuffer,
}

// ── App State ──────────────────────────────────────────────────────────

pub struct AppState {
    pub screen: Screen,
    pub focus: Pane,
    pub running: bool,
    pub show_help: bool,
    pub status_message: Option<String>,
    pub last_refresh: Option<DateTime<Utc>>,
    pub data: DataSnapshot,
    pub filter_text: String,
    pub filter_active: bool,

    // Per-screen selection indices
    pub session_index: usize,
    pub artifact_index: usize,
    pub review_index: usize,
    pub log_session_index: usize,
    pub log_scroll: usize,
    pub log_source: LogSource,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            screen: Screen::Overview,
            focus: Pane::Left,
            running: true,
            show_help: false,
            status_message: None,
            last_refresh: None,
            data: DataSnapshot::default(),
            filter_text: String::new(),
            filter_active: false,
            session_index: 0,
            artifact_index: 0,
            review_index: 0,
            log_session_index: 0,
            log_scroll: 0,
            log_source: LogSource::Stdout,
        }
    }

    pub fn selected_session_detail(&self) -> Option<&SessionDetail> {
        self.data.session_details.get(self.session_index)
    }

    pub fn selected_artifact(&self) -> Option<&ArtifactRow> {
        self.data.artifacts.get(self.artifact_index)
    }

    pub fn selected_review_detail(&self) -> Option<&ReviewDetail> {
        self.data.review_details.get(self.review_index)
    }

    pub fn log_session(&self) -> Option<&SessionRow> {
        self.data.sessions.get(self.log_session_index)
    }

    pub fn clamp_indices(&mut self) {
        let s = self.data.sessions.len();
        if s == 0 {
            self.session_index = 0;
        } else if self.session_index >= s {
            self.session_index = s - 1;
        }

        let a = self.data.artifacts.len();
        if a == 0 {
            self.artifact_index = 0;
        } else if self.artifact_index >= a {
            self.artifact_index = a - 1;
        }

        let r = self.data.reviews.len();
        if r == 0 {
            self.review_index = 0;
        } else if self.review_index >= r {
            self.review_index = r - 1;
        }

        if s == 0 {
            self.log_session_index = 0;
        } else if self.log_session_index >= s {
            self.log_session_index = s - 1;
        }
    }

    pub fn current_list_len(&self) -> usize {
        match self.screen {
            Screen::Overview => 0,
            Screen::Sessions => self.data.sessions.len(),
            Screen::Logs => {
                if self.focus == Pane::Left {
                    self.data.sessions.len()
                } else {
                    self.data.log_buffer.lines.len()
                }
            }
            Screen::Artifacts => self.data.artifacts.len(),
            Screen::Reviews => self.data.reviews.len(),
        }
    }

    pub fn current_index(&self) -> usize {
        match self.screen {
            Screen::Overview => 0,
            Screen::Sessions => self.session_index,
            Screen::Logs => {
                if self.focus == Pane::Left {
                    self.log_session_index
                } else {
                    self.log_scroll
                }
            }
            Screen::Artifacts => self.artifact_index,
            Screen::Reviews => self.review_index,
        }
    }

    pub fn set_current_index(&mut self, idx: usize) {
        match self.screen {
            Screen::Overview => {}
            Screen::Sessions => self.session_index = idx,
            Screen::Logs => {
                if self.focus == Pane::Left {
                    self.log_session_index = idx;
                } else {
                    self.log_scroll = idx;
                }
            }
            Screen::Artifacts => self.artifact_index = idx,
            Screen::Reviews => self.review_index = idx,
        }
    }
}

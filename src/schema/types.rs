use camino::Utf8PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version for forward compatibility.
pub const SCHEMA_VERSION: u32 = 1;

// ── Session Types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    InteractivePrimary,
    Reviewer,
    Tester,
    Committer,
    CiWatcher,
    E2eRunner,
    ShellUtility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Stopped,
    Crashed,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub id: Uuid,
    pub provider: String,
    pub role: SessionRole,
    pub model: Option<String>,
    pub status: SessionStatus,
    pub pid: Option<u32>,
    pub cwd: Utf8PathBuf,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub artifact_dir: Utf8PathBuf,
    pub log_dir: Utf8PathBuf,
}

impl SessionRecord {
    pub fn new(
        provider: String,
        role: SessionRole,
        model: Option<String>,
        cwd: Utf8PathBuf,
        artifact_dir: Utf8PathBuf,
        log_dir: Utf8PathBuf,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: Uuid::new_v4(),
            provider,
            role,
            model,
            status: SessionStatus::Running,
            pid: None,
            cwd,
            started_at: Utc::now(),
            stopped_at: None,
            artifact_dir,
            log_dir,
        }
    }
}

// ── Run Record ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    pub id: Uuid,
    pub session_id: Uuid,
    pub provider: String,
    pub role: SessionRole,
    pub model: Option<String>,
    pub command: Vec<String>,
    pub cwd: Utf8PathBuf,
    pub env_allowlist: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub artifact_paths: Vec<Utf8PathBuf>,
}

// ── Artifacts ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub id: Uuid,
    pub session_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub artifact_type: ArtifactType,
    pub path: Utf8PathBuf,
    pub git_context: Option<GitContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    LastResponse,
    FullConversation,
    Diff,
    StagedChanges,
    ReviewReport,
    TestReport,
    CommitProposal,
    CiSnapshot,
    HandoffManifest,
    Log,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitContext {
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub is_dirty: bool,
    pub diff_stat: Option<String>,
}

// ── Handoff ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffScope {
    LastResponse,
    FullConversation,
    CurrentDiff,
    StagedChanges,
    RepoSnapshot,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMode {
    ReadOnly,
    WorkspaceWrite,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffManifest {
    pub schema_version: u32,
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub source_session: Option<Uuid>,
    pub target_provider: String,
    pub target_role: SessionRole,
    pub goal: String,
    pub scope: HandoffScope,
    pub artifact_paths: Vec<Utf8PathBuf>,
    pub git_context: Option<GitContext>,
    pub model_override: Option<String>,
    pub expected_output_schema: Option<String>,
    pub safety_mode: SafetyMode,
}

// ── Review ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    NeedsWork,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    pub schema_version: u32,
    pub id: Uuid,
    pub handoff_id: Uuid,
    pub provider: String,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
    pub verdict: Verdict,
    pub raw_output: Option<String>,
}

// ── Test ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCommandResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub schema_version: u32,
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub commands_run: Vec<TestCommandResult>,
    pub failures: Vec<String>,
    pub verdict: Verdict,
}

// ── Commit ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitProposal {
    pub schema_version: u32,
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub proposed_message: String,
    pub risk_notes: Vec<String>,
    pub files_changed: Vec<String>,
    pub diff_stat: String,
}

// ── CI ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStatusSnapshot {
    pub schema_version: u32,
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub failed_jobs: Vec<String>,
    pub next_action: Option<String>,
    pub raw_output: Option<String>,
}

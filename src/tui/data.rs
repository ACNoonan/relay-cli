use camino::Utf8PathBuf;

use crate::artifacts;
use crate::git;
use crate::handoff;
use crate::provider;
use crate::schema::{ReviewResult, SessionStatus};
use crate::session;
use crate::storage::Storage;

use super::state::*;

/// Build a full data snapshot from the harness storage directory.
pub fn load_snapshot(harness_root: &Utf8PathBuf) -> DataSnapshot {
    let storage = Storage::new(harness_root.clone());
    let initialized = storage.is_initialized();

    let overview = build_overview(&storage, initialized);
    let (sessions, session_details) = build_sessions(&storage);
    let artifacts_rows = build_artifacts(&storage);
    let (reviews, review_details) = build_reviews(&storage);

    DataSnapshot {
        overview,
        sessions,
        session_details,
        artifacts: artifacts_rows,
        reviews,
        review_details,
        log_buffer: LogBuffer::default(),
    }
}

/// Load log lines for a specific session.
pub fn load_logs(harness_root: &Utf8PathBuf, session_id: uuid::Uuid, source: LogSource) -> LogBuffer {
    let storage = Storage::new(harness_root.clone());
    let path = match source {
        LogSource::Stdout => storage.session_stdout_path(session_id),
        LogSource::Stderr => storage.session_stderr_path(session_id),
    };

    let lines = if path.as_std_path().is_file() {
        match std::fs::read_to_string(path.as_std_path()) {
            Ok(content) => content.lines().map(|l| strip_ansi(l)).collect(),
            Err(_) => vec![],
        }
    } else {
        vec![]
    };

    LogBuffer { lines, source }
}

/// Simple ANSI escape stripping.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip CSI sequences: ESC [ ... final_byte
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc.is_ascii_alphabetic() || nc == '~' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn build_overview(storage: &Storage, initialized: bool) -> OverviewSnapshot {
    let git_repo = git::is_git_repo();
    let git_branch = git::current_branch().ok().flatten();
    let git_dirty = git::is_dirty().unwrap_or(false);

    let provider_checks = provider::all_providers()
        .iter()
        .map(|p| {
            let install = p.validate_installation();
            let auth = p.validate_auth();
            ProviderCheckRow {
                name: p.name().to_string(),
                installed: install.ok,
                auth: auth.ok,
            }
        })
        .collect();

    let sessions = session::list_sessions(storage).unwrap_or_default();
    let session_counts = SessionCounts {
        running: sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Running)
            .count(),
        completed: sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Completed)
            .count(),
        crashed: sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Crashed)
            .count(),
        stopped: sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Stopped)
            .count(),
    };

    let recent_sessions: Vec<SessionRow> = sessions
        .iter()
        .take(5)
        .map(|s| session_to_row(s))
        .collect();

    let all_artifacts = artifacts::list_artifacts(storage).unwrap_or_default();
    let recent_artifacts: Vec<ArtifactRow> = all_artifacts
        .iter()
        .take(5)
        .map(|a| artifact_to_row(a))
        .collect();

    let (recent_reviews, _) = build_reviews(storage);
    let recent_reviews: Vec<ReviewRow> = recent_reviews.into_iter().take(5).collect();

    OverviewSnapshot {
        harness_initialized: initialized,
        git_repo,
        git_branch,
        git_dirty,
        provider_checks,
        session_counts,
        recent_sessions,
        recent_artifacts,
        recent_reviews,
    }
}

fn build_sessions(storage: &Storage) -> (Vec<SessionRow>, Vec<SessionDetail>) {
    let sessions = session::list_sessions(storage).unwrap_or_default();
    let rows: Vec<SessionRow> = sessions.iter().map(|s| session_to_row(s)).collect();
    let details: Vec<SessionDetail> = sessions.iter().map(|s| session_to_detail(s)).collect();
    (rows, details)
}

fn build_artifacts(storage: &Storage) -> Vec<ArtifactRow> {
    let all = artifacts::list_artifacts(storage).unwrap_or_default();
    all.iter().map(|a| artifact_to_row(a)).collect()
}

fn build_reviews(storage: &Storage) -> (Vec<ReviewRow>, Vec<ReviewDetail>) {
    let handoffs = handoff::list_handoffs(storage).unwrap_or_default();
    let mut rows = Vec::new();
    let mut details = Vec::new();

    for h in &handoffs {
        if h.target_role != crate::schema::SessionRole::Reviewer {
            continue;
        }
        let result_path = storage.handoff_result_json_path(h.id);
        if !result_path.as_std_path().is_file() {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(result_path.as_std_path()) else {
            continue;
        };
        let Ok(result) = serde_json::from_str::<ReviewResult>(&data) else {
            continue;
        };

        rows.push(ReviewRow {
            id: result.id,
            short_id: result.id.to_string()[..8].to_string(),
            provider: result.provider.clone(),
            verdict: format!("{:?}", result.verdict),
            created_at: result.created_at,
            goal: h.goal.clone(),
            summary: result.summary.clone(),
            finding_count: result.findings.len(),
        });

        details.push(ReviewDetail {
            id: result.id,
            provider: result.provider.clone(),
            model: result.model.clone(),
            verdict: format!("{:?}", result.verdict),
            created_at: result.created_at,
            summary: result.summary.clone(),
            findings: result
                .findings
                .iter()
                .map(|f| FindingRow {
                    severity: f.severity.clone(),
                    category: f.category.clone(),
                    file: f.file.clone(),
                    line: f.line,
                    message: f.message.clone(),
                    suggestion: f.suggestion.clone(),
                })
                .collect(),
        });
    }

    (rows, details)
}

fn session_to_row(s: &crate::schema::SessionRecord) -> SessionRow {
    SessionRow {
        id: s.id,
        short_id: s.id.to_string()[..8].to_string(),
        provider: s.provider.clone(),
        role: format!("{:?}", s.role),
        status: format!("{:?}", s.status),
        started_at: s.started_at,
        model: s.model.clone(),
    }
}

fn session_to_detail(s: &crate::schema::SessionRecord) -> SessionDetail {
    SessionDetail {
        id: s.id,
        provider: s.provider.clone(),
        role: format!("{:?}", s.role),
        status: format!("{:?}", s.status),
        model: s.model.clone(),
        cwd: s.cwd.to_string(),
        started_at: s.started_at,
        stopped_at: s.stopped_at,
        artifact_dir: s.artifact_dir.to_string(),
        log_dir: s.log_dir.to_string(),
    }
}

fn artifact_to_row(a: &crate::schema::ArtifactManifest) -> ArtifactRow {
    ArtifactRow {
        id: a.id,
        short_id: a.id.to_string()[..8].to_string(),
        artifact_type: format!("{:?}", a.artifact_type),
        created_at: a.created_at,
        path: a.path.to_string(),
    }
}

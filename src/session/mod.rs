use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use uuid::Uuid;

use crate::process;
use crate::schema::{SessionRecord, SessionRole, SessionStatus};
use crate::storage::Storage;

/// Start a new interactive session.
pub async fn start_session(
    storage: &Storage,
    provider_name: &str,
    role: SessionRole,
    model: Option<String>,
    launch_cmd: Vec<String>,
) -> Result<SessionRecord> {
    let cwd = Utf8PathBuf::from(
        std::env::current_dir()
            .context("getting cwd")?
            .to_string_lossy()
            .to_string(),
    );

    let mut record = SessionRecord::new(
        provider_name.to_string(),
        role,
        model,
        cwd.clone(),
        storage.session_dir(Uuid::nil()), // placeholder, updated below
        storage.logs_dir(),
    );

    let session_dir = storage.session_dir(record.id);
    record.artifact_dir = session_dir.clone();
    record.log_dir = session_dir.clone();

    // Create session directory.
    std::fs::create_dir_all(session_dir.as_std_path())
        .with_context(|| format!("creating session dir {session_dir}"))?;

    // Save session record.
    save_record(storage, &record)?;

    // Launch the interactive process.
    let exit_code = process::spawn_interactive(&launch_cmd, Some(cwd.as_str())).await?;

    // Update record on exit.
    record.status = if exit_code == 0 {
        SessionStatus::Completed
    } else {
        SessionStatus::Crashed
    };
    record.stopped_at = Some(chrono::Utc::now());
    save_record(storage, &record)?;

    Ok(record)
}

/// List all sessions from storage.
pub fn list_sessions(storage: &Storage) -> Result<Vec<SessionRecord>> {
    let ids = storage.list_sessions()?;
    let mut records = Vec::new();
    for id in ids {
        let path = storage.session_record_path(id);
        if path.as_std_path().is_file() {
            let data = std::fs::read_to_string(path.as_std_path())
                .with_context(|| format!("reading session {id}"))?;
            if let Ok(record) = serde_json::from_str::<SessionRecord>(&data) {
                records.push(record);
            }
        }
    }
    records.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(records)
}

/// Stop a session by marking it as stopped.
pub fn stop_session(storage: &Storage, id: Uuid) -> Result<SessionRecord> {
    let path = storage.session_record_path(id);
    let data = std::fs::read_to_string(path.as_std_path())
        .with_context(|| format!("reading session {id}"))?;
    let mut record: SessionRecord =
        serde_json::from_str(&data).with_context(|| format!("parsing session {id}"))?;
    record.status = SessionStatus::Stopped;
    record.stopped_at = Some(chrono::Utc::now());
    save_record(storage, &record)?;
    Ok(record)
}

/// Load a single session record.
pub fn load_session(storage: &Storage, id: Uuid) -> Result<SessionRecord> {
    let path = storage.session_record_path(id);
    let data = std::fs::read_to_string(path.as_std_path())
        .with_context(|| format!("reading session {id}"))?;
    serde_json::from_str(&data).with_context(|| format!("parsing session {id}"))
}

/// Find the most recent session.
pub fn latest_session(storage: &Storage) -> Result<Option<SessionRecord>> {
    let records = list_sessions(storage)?;
    Ok(records.into_iter().next())
}

fn save_record(storage: &Storage, record: &SessionRecord) -> Result<()> {
    let path = storage.session_record_path(record.id);
    let data = serde_json::to_string_pretty(record).context("serializing session")?;
    std::fs::write(path.as_std_path(), data).context("writing session record")?;
    Ok(())
}

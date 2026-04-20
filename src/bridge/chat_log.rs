//! Per-session diagnostic log file.
//!
//! The TUI renders in the alternate screen buffer, which means any `tracing` /
//! `eprintln!` output is invisible. To make post-hoc debugging possible, each
//! `relay chat` session writes structured events to
//! `.agent-harness/logs/relay-chat-<ts>-<session-uuid>.log`, including:
//!
//! - every `WorkerEvent` the TUI receives (conversation snapshots, status changes,
//!   errors, status messages),
//! - every `BackendEvent` the worker observes (deltas, session updates, started/finished/error),
//! - subprocess stderr from Claude and Codex, flushed after each turn,
//! - the user's typed prompts and all rotation / new-conversation commands.
//!
//! If the harness isn't initialised, the log is disabled silently.

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::storage::Storage;

#[derive(Clone)]
pub struct ChatLog {
    inner: Arc<Mutex<Option<ChatLogInner>>>,
}

struct ChatLogInner {
    path: Utf8PathBuf,
}

impl ChatLog {
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Create (or continue) a log under `.agent-harness/logs/` for this chat session.
    /// Returns a disabled log on any failure — we never fail the TUI because logging broke.
    pub fn open(harness_root: Option<&Utf8Path>, session_uuid: uuid::Uuid) -> Self {
        let Some(root) = harness_root else {
            return Self::disabled();
        };
        let storage = Storage::new(root.to_path_buf());
        if !storage.is_initialized() {
            return Self::disabled();
        }
        let logs_dir = storage.logs_dir();
        if let Err(err) = fs::create_dir_all(logs_dir.as_std_path()) {
            tracing::warn!(%err, "could not create logs dir; chat log disabled");
            return Self::disabled();
        }
        let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
        let path = logs_dir.join(format!("relay-chat-{ts}-{session_uuid}.log"));

        let log = Self {
            inner: Arc::new(Mutex::new(Some(ChatLogInner { path: path.clone() }))),
        };
        log.write_line(
            "session",
            &json!({
                "session_uuid": session_uuid,
                "started_at": Utc::now().to_rfc3339(),
                "log_path": path.to_string(),
            }),
        );
        log
    }

    pub fn path(&self) -> Option<Utf8PathBuf> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|i| i.path.clone()))
    }

    /// Write a single structured record. Format: `{"ts":..,"kind":..,"payload":{...}}\n`.
    pub fn write<T: Serialize>(&self, kind: &str, payload: &T) {
        self.write_line(kind, payload);
    }

    fn write_line<T: Serialize>(&self, kind: &str, payload: &T) {
        let Ok(guard) = self.inner.lock() else { return };
        let Some(inner) = guard.as_ref() else { return };
        let record = json!({
            "ts": Utc::now().to_rfc3339(),
            "kind": kind,
            "payload": payload,
        });
        if let Err(err) = append_line(&inner.path, &record.to_string()) {
            // If the log file itself is broken, we do not retry — only log once.
            tracing::warn!(%err, path = %inner.path, "chat log write failed");
        }
    }

    /// Append a human-readable stderr block tagged by agent.
    pub fn write_subprocess_stderr(&self, agent: &str, stderr: &str) {
        if stderr.trim().is_empty() {
            return;
        }
        self.write(
            "subprocess_stderr",
            &json!({
                "agent": agent,
                "stderr": stderr,
            }),
        );
    }
}

fn append_line(path: &Utf8Path, line: &str) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.as_std_path())?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use uuid::Uuid;

pub mod migrations;

/// All storage paths under .agent-harness/.
pub struct Storage {
    root: Utf8PathBuf,
}

impl Storage {
    pub fn new(root: Utf8PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    pub fn config_path(&self) -> Utf8PathBuf {
        self.root.join("config.toml")
    }

    pub fn sessions_dir(&self) -> Utf8PathBuf {
        self.root.join("sessions")
    }

    pub fn runs_dir(&self) -> Utf8PathBuf {
        self.root.join("runs")
    }

    pub fn handoffs_dir(&self) -> Utf8PathBuf {
        self.root.join("handoffs")
    }

    pub fn artifacts_dir(&self) -> Utf8PathBuf {
        self.root.join("artifacts")
    }

    pub fn logs_dir(&self) -> Utf8PathBuf {
        self.root.join("logs")
    }

    pub fn cache_dir(&self) -> Utf8PathBuf {
        self.root.join("cache")
    }

    pub fn conversations_dir(&self) -> Utf8PathBuf {
        self.root.join("conversations")
    }

    pub fn conversation_dir(&self, id: Uuid) -> Utf8PathBuf {
        self.conversations_dir().join(id.to_string())
    }

    pub fn conversation_json_path(&self, id: Uuid) -> Utf8PathBuf {
        self.conversation_dir(id).join("conversation.json")
    }

    pub fn conversation_markdown_path(&self, id: Uuid) -> Utf8PathBuf {
        self.conversation_dir(id).join("transcript.md")
    }

    // Per-session paths

    pub fn session_dir(&self, id: Uuid) -> Utf8PathBuf {
        self.sessions_dir().join(id.to_string())
    }

    pub fn session_record_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("session.json")
    }

    pub fn session_events_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("events.jsonl")
    }

    pub fn session_latest_response_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("latest-response.md")
    }

    pub fn session_conversation_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("full-conversation.md")
    }

    pub fn session_stdout_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("stdout.log")
    }

    pub fn session_stderr_path(&self, id: Uuid) -> Utf8PathBuf {
        self.session_dir(id).join("stderr.log")
    }

    // Per-handoff paths

    pub fn handoff_dir(&self, id: Uuid) -> Utf8PathBuf {
        self.handoffs_dir().join(id.to_string())
    }

    pub fn handoff_manifest_path(&self, id: Uuid) -> Utf8PathBuf {
        self.handoff_dir(id).join("handoff.json")
    }

    pub fn handoff_prompt_path(&self, id: Uuid) -> Utf8PathBuf {
        self.handoff_dir(id).join("prompt.md")
    }

    pub fn handoff_result_json_path(&self, id: Uuid) -> Utf8PathBuf {
        self.handoff_dir(id).join("result.json")
    }

    pub fn handoff_result_md_path(&self, id: Uuid) -> Utf8PathBuf {
        self.handoff_dir(id).join("result.md")
    }

    /// Create all required directories.
    pub fn initialize(&self) -> Result<()> {
        let dirs = [
            self.sessions_dir(),
            self.runs_dir(),
            self.handoffs_dir(),
            self.artifacts_dir(),
            self.logs_dir(),
            self.cache_dir(),
            self.conversations_dir(),
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir.as_std_path())
                .with_context(|| format!("creating directory {dir}"))?;
        }
        Ok(())
    }

    /// Check if storage is initialized.
    pub fn is_initialized(&self) -> bool {
        self.root.as_std_path().is_dir() && self.config_path().as_std_path().is_file()
    }

    /// List all session directories.
    pub fn list_sessions(&self) -> Result<Vec<Uuid>> {
        let dir = self.sessions_dir();
        if !dir.as_std_path().is_dir() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(dir.as_std_path()).context("reading sessions dir")? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = Uuid::parse_str(name) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn list_conversations(&self) -> Result<Vec<Uuid>> {
        let dir = self.conversations_dir();
        if !dir.as_std_path().is_dir() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(dir.as_std_path()).context("reading conversations dir")? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = Uuid::parse_str(name) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// List all handoff directories.
    pub fn list_handoffs(&self) -> Result<Vec<Uuid>> {
        let dir = self.handoffs_dir();
        if !dir.as_std_path().is_dir() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(dir.as_std_path()).context("reading handoffs dir")? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = Uuid::parse_str(name) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}

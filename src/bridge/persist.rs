//! Conversation persistence: save JSON + human-readable markdown transcripts
//! under `.agent-harness/conversations/<uuid>/`.
//!
//! Writes are best-effort and never fail a running turn. If the harness is
//! not initialised (no `.agent-harness` present) persistence is silently
//! disabled and conversations live only in memory for the session.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use std::fs;

use super::conversation::{Conversation, Role, TurnStatus};
use crate::storage::Storage;

pub struct ConversationStore {
    storage: Option<Storage>,
}

impl ConversationStore {
    /// Construct a store if the given harness root is initialised; otherwise a no-op store.
    pub fn open(root: Option<Utf8PathBuf>) -> Self {
        let storage = root.map(Storage::new).filter(|s| s.is_initialized());
        Self { storage }
    }

    pub fn is_enabled(&self) -> bool {
        self.storage.is_some()
    }

    pub fn save(&self, conv: &Conversation) -> Result<()> {
        let Some(storage) = &self.storage else {
            return Ok(());
        };
        let dir = storage.conversation_dir(conv.id);
        fs::create_dir_all(dir.as_std_path())
            .with_context(|| format!("creating conversation dir {dir}"))?;

        let json_path = storage.conversation_json_path(conv.id);
        let json = serde_json::to_vec_pretty(conv).context("serialising conversation")?;
        fs::write(json_path.as_std_path(), json)
            .with_context(|| format!("writing conversation json {json_path}"))?;

        let md_path = storage.conversation_markdown_path(conv.id);
        fs::write(md_path.as_std_path(), render_markdown(conv))
            .with_context(|| format!("writing transcript {md_path}"))?;

        Ok(())
    }

    pub fn load(&self, id: uuid::Uuid) -> Result<Conversation> {
        let Some(storage) = &self.storage else {
            anyhow::bail!("no harness initialised; cannot load conversation {id}");
        };
        let path = storage.conversation_json_path(id);
        let bytes =
            fs::read(path.as_std_path()).with_context(|| format!("reading conversation {path}"))?;
        let mut conv: Conversation =
            serde_json::from_slice(&bytes).context("parsing conversation json")?;
        // Bring older on-disk formats up to the current shape transparently. This is the
        // boundary where we honour the `CONVERSATION_SCHEMA_VERSION` contract.
        conv.upgrade_in_place();
        Ok(conv)
    }
}

fn render_markdown(conv: &Conversation) -> String {
    let mut out = String::new();
    out.push_str(&format!("# conversation {}\n\n", conv.id));
    out.push_str(&format!(
        "created: {}\nupdated: {}\nactive: {}\nauto-handoff: {}\n\n",
        conv.created_at.to_rfc3339(),
        conv.updated_at.to_rfc3339(),
        conv.active_agent.label(),
        conv.auto_handoff_enabled
    ));
    if let Some(cid) = &conv.sessions.claude_session_id {
        out.push_str(&format!("claude session: `{cid}`\n"));
    }
    if let Some(tid) = &conv.sessions.codex_thread_id {
        out.push_str(&format!("codex thread: `{tid}`\n"));
    }
    out.push('\n');
    for turn in &conv.turns {
        let heading = match turn.role {
            Role::User => "## you".to_string(),
            Role::Handoff => format!("## ↪ handoff → {}", turn.agent.label()),
            Role::Assistant => format!("## {}", turn.agent.label()),
            Role::System => "## system".to_string(),
        };
        out.push_str(&heading);
        if turn.status == TurnStatus::Error {
            out.push_str("  _(error)_");
        } else if turn.status == TurnStatus::Streaming {
            out.push_str("  _(interrupted while streaming)_");
        }
        out.push_str(&format!(
            "\n_{}_\n\n{}\n\n",
            turn.ts.to_rfc3339(),
            turn.content
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::conversation::{Agent, Role, Turn, TurnStatus, CONVERSATION_SCHEMA_VERSION};
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn store_in(tmp: &TempDir) -> ConversationStore {
        let root =
            Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("tempdir path is utf-8");
        // Storage::is_initialized() looks for config.toml — write an empty one so the
        // store enables persistence.
        std::fs::write(root.join("config.toml").as_std_path(), b"").unwrap();
        std::fs::create_dir_all(root.join("conversations").as_std_path()).unwrap();
        ConversationStore::open(Some(root))
    }

    #[test]
    fn compacted_conversation_round_trips() {
        let tmp = TempDir::new().expect("tempdir");
        let store = store_in(&tmp);

        let mut conv = Conversation::new(Agent::Gpt, true);
        conv.turns.push(Turn::new(
            Agent::Gpt,
            Role::User,
            "first thing",
            TurnStatus::Complete,
        ));
        conv.turns
            .push(Turn::new_summary("rolled-up earlier work", 12));
        conv.turns.push(Turn::new(
            Agent::Gpt,
            Role::Assistant,
            "current reply",
            TurnStatus::Complete,
        ));
        conv.summary = Some("rolled-up earlier work".into());

        store.save(&conv).expect("save");
        let loaded = store.load(conv.id).expect("load");

        assert_eq!(loaded.schema_version, CONVERSATION_SCHEMA_VERSION);
        assert_eq!(loaded.turns.len(), 3);
        assert!(loaded.turns[1].is_summary());
        assert_eq!(loaded.turns[1].summarized_turn_count, Some(12));
        assert_eq!(loaded.summary.as_deref(), Some("rolled-up earlier work"));
    }
}

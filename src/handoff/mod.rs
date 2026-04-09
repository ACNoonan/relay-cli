use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use chrono::Utc;
use uuid::Uuid;

use crate::schema::{
    GitContext, HandoffManifest, HandoffScope, SafetyMode, SessionRole, SCHEMA_VERSION,
};
use crate::storage::Storage;

/// Create a handoff manifest and persist it.
#[allow(clippy::too_many_arguments)]
pub fn create_handoff(
    storage: &Storage,
    source_session: Option<Uuid>,
    target_provider: &str,
    target_role: SessionRole,
    goal: &str,
    scope: HandoffScope,
    artifact_paths: Vec<Utf8PathBuf>,
    git_context: Option<GitContext>,
    model_override: Option<String>,
    safety_mode: SafetyMode,
) -> Result<HandoffManifest> {
    let manifest = HandoffManifest {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        source_session,
        target_provider: target_provider.to_string(),
        target_role,
        goal: goal.to_string(),
        scope,
        artifact_paths,
        git_context,
        model_override,
        expected_output_schema: Some("review_result".to_string()),
        safety_mode,
    };

    let handoff_dir = storage.handoff_dir(manifest.id);
    std::fs::create_dir_all(handoff_dir.as_std_path())
        .with_context(|| format!("creating handoff dir {handoff_dir}"))?;

    let path = storage.handoff_manifest_path(manifest.id);
    let json = serde_json::to_string_pretty(&manifest).context("serializing handoff")?;
    std::fs::write(path.as_std_path(), json).context("writing handoff manifest")?;

    Ok(manifest)
}

/// Load a handoff manifest.
pub fn load_handoff(storage: &Storage, id: Uuid) -> Result<HandoffManifest> {
    let path = storage.handoff_manifest_path(id);
    let data = std::fs::read_to_string(path.as_std_path())
        .with_context(|| format!("reading handoff {id}"))?;
    serde_json::from_str(&data).with_context(|| format!("parsing handoff {id}"))
}

/// List all handoffs.
pub fn list_handoffs(storage: &Storage) -> Result<Vec<HandoffManifest>> {
    let ids = storage.list_handoffs()?;
    let mut manifests = Vec::new();
    for id in ids {
        match load_handoff(storage, id) {
            Ok(m) => manifests.push(m),
            Err(_) => continue,
        }
    }
    manifests.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(manifests)
}

/// Build the review prompt for a handoff.
pub fn build_review_prompt(handoff: &HandoffManifest, content: &str) -> String {
    format!(
        r#"You are a code reviewer. Your task: {goal}

## Scope
{scope:?}

## Content to Review

{content}

## Instructions
- Analyze the content carefully.
- Identify bugs, security issues, performance problems, and code quality concerns.
- For each finding, provide: severity (critical/high/medium/low/info), category, file, line (if applicable), message, and suggestion.
- Produce a verdict: pass, fail, needs_work, or inconclusive.

## Required Output Format (JSON)
```json
{{
  "summary": "...",
  "findings": [
    {{
      "severity": "high",
      "category": "bug",
      "file": "src/main.rs",
      "line": 42,
      "message": "...",
      "suggestion": "..."
    }}
  ],
  "verdict": "needs_work"
}}
```

Respond ONLY with the JSON block, no surrounding text."#,
        goal = handoff.goal,
        scope = handoff.scope,
        content = content,
    )
}

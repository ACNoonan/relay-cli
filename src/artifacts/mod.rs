use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use chrono::Utc;
use uuid::Uuid;

use crate::schema::{ArtifactManifest, ArtifactType, GitContext};
use crate::storage::Storage;

/// Save an artifact and return its manifest.
pub fn save_artifact(
    storage: &Storage,
    session_id: Uuid,
    artifact_type: ArtifactType,
    content: &str,
    git_context: Option<GitContext>,
) -> Result<ArtifactManifest> {
    let id = Uuid::new_v4();
    let filename = match artifact_type {
        ArtifactType::LastResponse => "latest-response.md",
        ArtifactType::FullConversation => "full-conversation.md",
        ArtifactType::Diff => "diff.patch",
        ArtifactType::StagedChanges => "staged.patch",
        ArtifactType::ReviewReport => "review-report.md",
        ArtifactType::TestReport => "test-report.md",
        ArtifactType::CommitProposal => "commit-proposal.md",
        ArtifactType::CiSnapshot => "ci-snapshot.json",
        ArtifactType::HandoffManifest => "handoff.json",
        ArtifactType::Log => "output.log",
    };

    let artifact_dir = storage.artifacts_dir().join(id.to_string());
    std::fs::create_dir_all(artifact_dir.as_std_path())
        .with_context(|| format!("creating artifact dir {artifact_dir}"))?;

    let file_path = artifact_dir.join(filename);
    std::fs::write(file_path.as_std_path(), content)
        .with_context(|| format!("writing artifact {file_path}"))?;

    let manifest = ArtifactManifest {
        schema_version: crate::schema::SCHEMA_VERSION,
        id,
        session_id,
        created_at: Utc::now(),
        artifact_type,
        path: file_path.clone(),
        git_context,
    };

    // Save manifest alongside artifact.
    let manifest_path = artifact_dir.join("manifest.json");
    let manifest_json = serde_json::to_string_pretty(&manifest).context("serializing manifest")?;
    std::fs::write(manifest_path.as_std_path(), manifest_json)
        .context("writing artifact manifest")?;

    Ok(manifest)
}

/// List all artifacts from storage.
pub fn list_artifacts(storage: &Storage) -> Result<Vec<ArtifactManifest>> {
    let dir = storage.artifacts_dir();
    if !dir.as_std_path().is_dir() {
        return Ok(vec![]);
    }

    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(dir.as_std_path()).context("reading artifacts dir")? {
        let entry = entry?;
        let manifest_path = Utf8PathBuf::from_path_buf(entry.path().join("manifest.json"))
            .unwrap_or_else(|p| Utf8PathBuf::from(p.to_string_lossy().to_string()));
        if manifest_path.as_std_path().is_file() {
            let data = std::fs::read_to_string(manifest_path.as_std_path())?;
            if let Ok(m) = serde_json::from_str::<ArtifactManifest>(&data) {
                manifests.push(m);
            }
        }
    }

    manifests.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(manifests)
}

/// Get the content of an artifact by ID.
pub fn read_artifact(storage: &Storage, id: Uuid) -> Result<(ArtifactManifest, String)> {
    let dir = storage.artifacts_dir().join(id.to_string());
    let manifest_path = dir.join("manifest.json");
    let data = std::fs::read_to_string(manifest_path.as_std_path())
        .with_context(|| format!("reading artifact manifest for {id}"))?;
    let manifest: ArtifactManifest =
        serde_json::from_str(&data).with_context(|| format!("parsing artifact manifest {id}"))?;

    let content = std::fs::read_to_string(manifest.path.as_std_path())
        .with_context(|| format!("reading artifact content at {}", manifest.path))?;

    Ok((manifest, content))
}

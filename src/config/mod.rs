use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    pub schema_version: u32,
    pub workspace: WorkspaceConfig,
    pub storage: StorageConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub roles: HashMap<String, RoleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_harness_dir")]
    pub harness_dir: String,
    #[serde(default)]
    pub git_worktree_enabled: bool,
}

fn default_harness_dir() -> String {
    ".agent-harness".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_max_artifact_mb")]
    pub max_artifact_mb: u32,
}

fn default_retention_days() -> u32 {
    30
}
fn default_max_artifact_mb() -> u32 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub binary: Option<String>,
    pub default_model: Option<String>,
    #[serde(default)]
    pub allowed_modes: Vec<String>,
    #[serde(default)]
    pub interactive_only: bool,
    #[serde(default)]
    pub non_interactive_enabled: bool,
    #[serde(default)]
    pub env_passthrough: Vec<String>,
    #[serde(default)]
    pub prompt_templates: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    pub provider: String,
    pub model: Option<String>,
    #[serde(default = "default_safety_mode")]
    pub safety_mode: String,
    pub prompt_template: Option<String>,
    #[serde(default)]
    pub test_commands: Vec<String>,
}

fn default_safety_mode() -> String {
    "read_only".to_string()
}

impl HarnessConfig {
    pub fn default_config() -> Result<Self> {
        toml::from_str(DEFAULT_CONFIG).context("failed to parse default config")
    }

    pub fn load(path: &Utf8Path) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading config at {path}"))?;
        toml::from_str(&contents).with_context(|| format!("parsing config at {path}"))
    }

    pub fn save(&self, path: &Utf8Path) -> Result<()> {
        let contents = toml::to_string_pretty(self).context("serializing config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent.as_std_path())
                .with_context(|| format!("creating dir {parent}"))?;
        }
        std::fs::write(path.as_std_path(), contents)
            .with_context(|| format!("writing config to {path}"))?;
        Ok(())
    }

    pub fn harness_dir(&self) -> Utf8PathBuf {
        Utf8PathBuf::from(&self.workspace.harness_dir)
    }

    pub fn provider_config(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn role_config(&self, name: &str) -> Option<&RoleConfig> {
        self.roles.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config_has_expected_contract() {
        let cfg = HarnessConfig::default_config().expect("default config should parse");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.workspace.harness_dir, ".agent-harness");
        assert_eq!(cfg.storage.retention_days, 30);
        assert_eq!(cfg.storage.max_artifact_mb, 100);
        assert!(cfg.provider_config("claude").is_some());
        assert!(cfg.provider_config("codex").is_some());
        assert!(cfg.role_config("reviewer").is_some());
        assert!(cfg.role_config("tester").is_some());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let path = Utf8PathBuf::from_path_buf(tmp.path().join("config.toml"))
            .expect("temp path should be valid UTF-8");
        let cfg = HarnessConfig::default_config().expect("default config should parse");
        cfg.save(&path).expect("config should save");

        let loaded = HarnessConfig::load(&path).expect("config should load");
        assert_eq!(loaded.schema_version, cfg.schema_version);
        assert_eq!(loaded.workspace.harness_dir, cfg.workspace.harness_dir);
        assert_eq!(loaded.storage.retention_days, cfg.storage.retention_days);
        assert_eq!(loaded.storage.max_artifact_mb, cfg.storage.max_artifact_mb);
        assert_eq!(loaded.providers.len(), cfg.providers.len());
        assert_eq!(loaded.roles.len(), cfg.roles.len());
    }

    #[test]
    fn load_invalid_toml_fails() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let path = Utf8PathBuf::from_path_buf(tmp.path().join("broken.toml"))
            .expect("temp path should be valid UTF-8");
        std::fs::write(path.as_std_path(), "schema_version = {").expect("write should succeed");

        let result = HarnessConfig::load(&path);
        assert!(result.is_err());
    }
}

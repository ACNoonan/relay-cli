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

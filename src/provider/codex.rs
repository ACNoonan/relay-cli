use super::{DoctorCheck, Provider, ProviderMode};
use anyhow::Result;
use std::collections::HashSet;

pub struct CodexProvider {
    binary: String,
}

impl CodexProvider {
    pub fn new(binary_override: Option<&str>) -> Self {
        Self {
            binary: binary_override.unwrap_or("codex").to_string(),
        }
    }
}

impl Provider for CodexProvider {
    fn name(&self) -> &str {
        "codex"
    }

    fn validate_installation(&self) -> DoctorCheck {
        let found = which::which(&self.binary).is_ok();
        DoctorCheck {
            name: format!("{} binary", self.name()),
            ok: found,
            message: if found {
                format!("`{}` found in PATH", self.binary)
            } else {
                format!(
                    "`{}` not found — install with `npm install -g @openai/codex`",
                    self.binary
                )
            },
            warning: false,
        }
    }

    fn validate_auth(&self) -> DoctorCheck {
        let has_key = std::env::var("OPENAI_API_KEY").is_ok();
        DoctorCheck {
            name: format!("{} auth", self.name()),
            ok: has_key,
            message: if has_key {
                "OPENAI_API_KEY is set".to_string()
            } else {
                "OPENAI_API_KEY not set — Codex requires an OpenAI API key".to_string()
            },
            warning: !has_key,
        }
    }

    fn supported_modes(&self) -> HashSet<ProviderMode> {
        let mut modes = HashSet::new();
        modes.insert(ProviderMode::NonInteractive);
        modes.insert(ProviderMode::Review);
        modes.insert(ProviderMode::Test);
        modes.insert(ProviderMode::Commit);
        modes
    }

    fn build_launch_command(&self, model: Option<&str>) -> Result<Vec<String>> {
        let mut cmd = vec![self.binary.clone()];
        if let Some(m) = model {
            cmd.push("--model".to_string());
            cmd.push(m.to_string());
        }
        Ok(cmd)
    }

    fn build_prompt_command(&self, prompt: &str, model: Option<&str>) -> Result<Vec<String>> {
        let mut cmd = vec![
            self.binary.clone(),
            "--quiet".to_string(),
            "--approval-mode".to_string(),
            "full-auto".to_string(),
        ];
        if let Some(m) = model {
            cmd.push("--model".to_string());
            cmd.push(m.to_string());
        }
        cmd.push(prompt.to_string());
        Ok(cmd)
    }

    fn safe_for_unattended(&self) -> bool {
        true
    }
}

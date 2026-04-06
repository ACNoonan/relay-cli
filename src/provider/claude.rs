use super::{DoctorCheck, Provider, ProviderMode};
use anyhow::Result;
use std::collections::HashSet;

pub struct ClaudeProvider {
    binary: String,
}

impl ClaudeProvider {
    pub fn new(binary_override: Option<&str>) -> Self {
        Self {
            binary: binary_override.unwrap_or("claude").to_string(),
        }
    }
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    fn validate_installation(&self) -> DoctorCheck {
        let found = which::which(&self.binary).is_ok();
        DoctorCheck {
            name: format!("{} binary", self.name()),
            ok: found,
            message: if found {
                format!("`{}` found in PATH", self.binary)
            } else {
                format!("`{}` not found — install Claude Code CLI", self.binary)
            },
            warning: false,
        }
    }

    fn validate_auth(&self) -> DoctorCheck {
        // Claude auth is handled through the CLI's own login flow.
        // We check if the config directory exists as a heuristic.
        let home = std::env::var("HOME").unwrap_or_default();
        let config_exists = std::path::Path::new(&home).join(".claude").is_dir();
        DoctorCheck {
            name: format!("{} auth", self.name()),
            ok: config_exists,
            message: if config_exists {
                "Claude config directory found (~/.claude)".to_string()
            } else {
                "No ~/.claude directory — run `claude` to authenticate".to_string()
            },
            warning: !config_exists,
        }
    }

    fn supported_modes(&self) -> HashSet<ProviderMode> {
        let mut modes = HashSet::new();
        modes.insert(ProviderMode::Interactive);
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
        // Non-interactive Claude is experimental and disabled by default.
        let mut cmd = vec![self.binary.clone(), "-p".to_string(), prompt.to_string()];
        if let Some(m) = model {
            cmd.push("--model".to_string());
            cmd.push(m.to_string());
        }
        Ok(cmd)
    }

    fn safe_for_unattended(&self) -> bool {
        // Claude non-interactive is NOT safe by default.
        false
    }

    fn doctor_checks(&self) -> Vec<DoctorCheck> {
        let mut checks = vec![self.validate_installation(), self.validate_auth()];
        checks.push(DoctorCheck {
            name: "claude non-interactive".to_string(),
            ok: true,
            message: "Non-interactive Claude (`-p`) is disabled by default. \
                      Enable with `providers.claude.non_interactive_enabled = true` in config — \
                      be aware of subscription/API cost implications."
                .to_string(),
            warning: true,
        });
        checks
    }
}

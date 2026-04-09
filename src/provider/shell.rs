use super::{DoctorCheck, Provider, ProviderMode};
use anyhow::Result;
use std::collections::HashSet;

pub struct ShellProvider {
    binary: String,
}

impl ShellProvider {
    pub fn new(binary_override: Option<&str>) -> Self {
        Self {
            binary: binary_override.unwrap_or("sh").to_string(),
        }
    }
}

impl Provider for ShellProvider {
    fn name(&self) -> &str {
        "shell"
    }

    fn validate_installation(&self) -> DoctorCheck {
        let found = which::which(&self.binary).is_ok();
        DoctorCheck {
            name: format!("{} binary", self.name()),
            ok: found,
            message: if found {
                format!("`{}` available", self.binary)
            } else {
                format!("`{}` not found", self.binary)
            },
            warning: false,
        }
    }

    fn validate_auth(&self) -> DoctorCheck {
        DoctorCheck {
            name: format!("{} auth", self.name()),
            ok: true,
            message: "Shell requires no auth".to_string(),
            warning: false,
        }
    }

    fn supported_modes(&self) -> HashSet<ProviderMode> {
        let mut modes = HashSet::new();
        modes.insert(ProviderMode::NonInteractive);
        modes.insert(ProviderMode::Test);
        modes.insert(ProviderMode::Ci);
        modes
    }

    fn build_launch_command(&self, _model: Option<&str>) -> Result<Vec<String>> {
        Ok(vec![self.binary.clone()])
    }

    fn build_prompt_command(&self, prompt: &str, _model: Option<&str>) -> Result<Vec<String>> {
        Ok(vec![
            self.binary.clone(),
            "-c".to_string(),
            prompt.to_string(),
        ])
    }

    fn safe_for_unattended(&self) -> bool {
        true
    }
}

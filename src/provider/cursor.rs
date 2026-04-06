use super::{DoctorCheck, Provider, ProviderMode};
use anyhow::Result;
use std::collections::HashSet;

pub struct CursorProvider {
    binary: String,
}

impl CursorProvider {
    pub fn new(binary_override: Option<&str>) -> Self {
        Self {
            binary: binary_override.unwrap_or("cursor").to_string(),
        }
    }
}

impl Provider for CursorProvider {
    fn name(&self) -> &str {
        "cursor"
    }

    fn validate_installation(&self) -> DoctorCheck {
        let found = which::which(&self.binary).is_ok();
        DoctorCheck {
            name: format!("{} binary", self.name()),
            ok: found,
            message: if found {
                format!("`{}` found in PATH", self.binary)
            } else {
                format!("`{}` not found — install Cursor IDE", self.binary)
            },
            warning: false,
        }
    }

    fn validate_auth(&self) -> DoctorCheck {
        // Cursor auth is managed by the IDE; we just check if the binary exists.
        let found = which::which(&self.binary).is_ok();
        DoctorCheck {
            name: format!("{} auth", self.name()),
            ok: found,
            message: if found {
                "Cursor auth is managed by the IDE".to_string()
            } else {
                "Cursor not installed — auth check skipped".to_string()
            },
            warning: false,
        }
    }

    fn supported_modes(&self) -> HashSet<ProviderMode> {
        let mut modes = HashSet::new();
        modes.insert(ProviderMode::Review);
        modes
    }

    fn build_launch_command(&self, _model: Option<&str>) -> Result<Vec<String>> {
        Ok(vec![self.binary.clone(), ".".to_string()])
    }

    fn build_prompt_command(&self, _prompt: &str, _model: Option<&str>) -> Result<Vec<String>> {
        anyhow::bail!("Cursor does not support non-interactive prompt mode")
    }

    fn safe_for_unattended(&self) -> bool {
        false
    }
}

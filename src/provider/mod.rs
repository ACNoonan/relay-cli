pub mod claude;
pub mod codex;
pub mod cursor;
pub mod shell;

use anyhow::Result;
use std::collections::HashSet;

/// Capabilities a provider may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderMode {
    Interactive,
    NonInteractive,
    Review,
    Test,
    Commit,
    Ci,
}

/// Outcome of a health check.
#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub message: String,
    pub warning: bool,
}

/// Provider adapter trait. Implementations are not required to support all methods.
pub trait Provider: Send + Sync {
    /// Provider name (e.g., "claude", "codex").
    fn name(&self) -> &str;

    /// Check if the CLI binary is installed and reachable.
    fn validate_installation(&self) -> DoctorCheck;

    /// Heuristically check if auth credentials are available.
    fn validate_auth(&self) -> DoctorCheck;

    /// Declared supported modes.
    fn supported_modes(&self) -> HashSet<ProviderMode>;

    /// Build the command to launch an interactive session.
    fn build_launch_command(&self, model: Option<&str>) -> Result<Vec<String>>;

    /// Build a command for a non-interactive run with a prompt.
    fn build_prompt_command(&self, prompt: &str, model: Option<&str>) -> Result<Vec<String>>;

    /// Whether this provider is safe for unattended automation.
    fn safe_for_unattended(&self) -> bool;

    /// Run all doctor checks for this provider.
    fn doctor_checks(&self) -> Vec<DoctorCheck> {
        vec![self.validate_installation(), self.validate_auth()]
    }
}

/// Resolve a provider by name.
pub fn get_provider(name: &str, binary_override: Option<&str>) -> Option<Box<dyn Provider>> {
    match name {
        "claude" => Some(Box::new(claude::ClaudeProvider::new(binary_override))),
        "codex" => Some(Box::new(codex::CodexProvider::new(binary_override))),
        "cursor" => Some(Box::new(cursor::CursorProvider::new(binary_override))),
        "shell" => Some(Box::new(shell::ShellProvider::new(binary_override))),
        _ => None,
    }
}

/// Get all known providers.
pub fn all_providers() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(claude::ClaudeProvider::new(None)),
        Box::new(codex::CodexProvider::new(None)),
        Box::new(cursor::CursorProvider::new(None)),
        Box::new(shell::ShellProvider::new(None)),
    ]
}

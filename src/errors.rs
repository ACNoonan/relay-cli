use thiserror::Error;

#[derive(Error, Debug)]
pub enum RelayError {
    #[error("not in a git repository")]
    NotGitRepo,

    #[error("harness not initialized — run `relay init` first")]
    NotInitialized,

    #[error("harness already initialized at {0}")]
    AlreadyInitialized(String),

    #[error("provider '{0}' not found — is it installed?")]
    ProviderNotFound(String),

    #[error("provider '{0}' auth not available")]
    ProviderAuthMissing(String),

    #[error("provider '{0}' does not support mode '{1}'")]
    UnsupportedMode(String, String),

    #[error("session '{0}' not found")]
    SessionNotFound(String),

    #[error("session '{0}' is already running")]
    SessionAlreadyRunning(String),

    #[error("no active session")]
    NoActiveSession,

    #[error("artifact not found: {0}")]
    ArtifactNotFound(String),

    #[error("handoff failed: {0}")]
    HandoffFailed(String),

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("process exited with code {code}: {message}")]
    ProcessFailed { code: i32, message: String },

    #[error("capture failed: {0}")]
    CaptureFailed(String),

    #[error("review parse error: {0}")]
    ReviewParseError(String),
}

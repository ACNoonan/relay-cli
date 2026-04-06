mod commands;

pub use commands::*;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "relay",
    about = "Local agent harness CLI — orchestrate Claude Code, Codex, Cursor, and utility agents",
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Override config file path
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize the harness in the current repository
    Init,

    /// Check provider installations, auth, and configuration
    Doctor,

    /// Manage agent sessions
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Capture artifacts from sessions
    Capture {
        #[command(subcommand)]
        action: CaptureAction,
    },

    /// Hand off artifacts to another agent
    Handoff {
        /// Target provider
        #[arg(long)]
        provider: String,
        /// Artifact ID to hand off
        #[arg(long)]
        artifact: String,
        /// Goal description
        #[arg(long)]
        goal: String,
        /// Model override
        #[arg(long)]
        model: Option<String>,
    },

    /// Run a code review via another agent
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },

    /// Run tests
    Test {
        #[command(subcommand)]
        action: TestAction,
    },

    /// Prepare and manage commits
    Commit {
        #[command(subcommand)]
        action: CommitAction,
    },

    /// Watch CI/CD status
    Ci {
        #[command(subcommand)]
        action: CiAction,
    },

    /// Run end-to-end tests
    E2e {
        /// Commands to run
        #[arg(long)]
        command: Vec<String>,
    },

    /// Browse and inspect artifacts
    Artifacts {
        #[command(subcommand)]
        action: ArtifactAction,
    },

    /// View logs
    Logs {
        /// Session ID to view logs for
        #[arg(long)]
        session: Option<String>,
        /// Number of lines
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Launch the interactive TUI dashboard
    Tui,
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// Start a new interactive session
    Start {
        /// Provider to use (default: claude)
        #[arg(default_value = "claude")]
        provider: String,
        /// Model override
        #[arg(long)]
        model: Option<String>,
    },
    /// List all sessions
    List,
    /// Stop a running session
    Stop {
        /// Session ID
        id: String,
    },
    /// Show details of a session
    Show {
        /// Session ID (or "latest")
        id: String,
    },
}

#[derive(Subcommand)]
pub enum CaptureAction {
    /// Capture the last response from a session
    LastResponse {
        /// Session ID (default: latest)
        #[arg(long)]
        session: Option<String>,
        /// Path to a file containing the response to capture
        #[arg(long)]
        file: Option<String>,
    },
    /// Capture the full conversation transcript
    Transcript {
        /// Session ID (default: latest)
        #[arg(long)]
        session: Option<String>,
        /// Path to a file containing the transcript
        #[arg(long)]
        file: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ReviewAction {
    /// Review using Codex
    Codex {
        /// Artifact ID to review (default: latest capture)
        #[arg(long)]
        artifact: Option<String>,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Goal description
        #[arg(long, default_value = "Review the code for bugs, security issues, and quality")]
        goal: String,
    },
    /// Review using Cursor
    Cursor {
        /// Artifact ID to review
        #[arg(long)]
        artifact: Option<String>,
    },
    /// Show review history
    History,
}

#[derive(Subcommand)]
pub enum TestAction {
    /// Run configured test commands
    Run {
        /// Override test commands
        #[arg(long)]
        command: Vec<String>,
    },
    /// Show test history
    History,
}

#[derive(Subcommand)]
pub enum CommitAction {
    /// Prepare a commit proposal
    Prepare,
}

#[derive(Subcommand)]
pub enum CiAction {
    /// Check current CI status
    Watch,
}

#[derive(Subcommand)]
pub enum ArtifactAction {
    /// List all artifacts
    List,
    /// Show a specific artifact
    Show {
        /// Artifact ID
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current config
    Show,
    /// Open config in editor
    Edit,
    /// Reset config to defaults
    Reset,
}

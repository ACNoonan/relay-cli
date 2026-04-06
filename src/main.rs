mod artifacts;
mod ci;
mod cli;
mod commit;
mod config;
mod e2e;
mod errors;
mod git;
mod handoff;
mod process;
mod provider;
mod review;
mod schema;
mod session;
mod storage;
mod testing;
mod tui;

use clap::Parser;
use cli::{
    ArtifactAction, CaptureAction, CiAction, Cli, Commands, CommitAction, ConfigAction,
    ReviewAction, SessionAction, TestAction,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize tracing.
    let filter = if cli.verbose {
        EnvFilter::new("relay_cli=debug")
    } else {
        EnvFilter::new("relay_cli=info")
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();

    match cli.command {
        Commands::Init => cli::cmd_init().await,
        Commands::Doctor => cli::cmd_doctor().await,

        Commands::Session { action } => match action {
            SessionAction::Start { provider, model } => {
                cli::cmd_session_start(&provider, model).await
            }
            SessionAction::List => cli::cmd_session_list().await,
            SessionAction::Stop { id } => cli::cmd_session_stop(&id).await,
            SessionAction::Show { id } => cli::cmd_session_show(&id).await,
        },

        Commands::Capture { action } => match action {
            CaptureAction::LastResponse { session, file } => {
                cli::cmd_capture_last_response(session, file).await
            }
            CaptureAction::Transcript { session, file } => {
                cli::cmd_capture_transcript(session, file).await
            }
        },

        Commands::Handoff {
            provider: _,
            artifact,
            goal,
            model,
        } => {
            // Generic handoff — delegates to review for now.
            cli::cmd_review_codex(Some(artifact), model, goal).await
        }

        Commands::Review { action } => match action {
            ReviewAction::Codex {
                artifact,
                model,
                goal,
            } => cli::cmd_review_codex(artifact, model, goal).await,
            ReviewAction::Cursor { .. } => {
                println!("Cursor review is not yet implemented.");
                Ok(())
            }
            ReviewAction::History => cli::cmd_review_history().await,
        },

        Commands::Test { action } => match action {
            TestAction::Run { command } => cli::cmd_test_run(command).await,
            TestAction::History => cli::cmd_test_history().await,
        },

        Commands::Commit { action } => match action {
            CommitAction::Prepare => cli::cmd_commit_prepare().await,
        },

        Commands::Ci { action } => match action {
            CiAction::Watch => cli::cmd_ci_watch().await,
        },

        Commands::E2e { command } => cli::cmd_e2e(command).await,

        Commands::Artifacts { action } => match action {
            ArtifactAction::List => cli::cmd_artifacts_list().await,
            ArtifactAction::Show { id } => cli::cmd_artifacts_show(&id).await,
        },

        Commands::Logs { session, lines } => cli::cmd_logs(session, lines).await,

        Commands::Config { action } => match action {
            ConfigAction::Show => cli::cmd_config_show().await,
            ConfigAction::Edit => cli::cmd_config_edit().await,
            ConfigAction::Reset => cli::cmd_config_reset().await,
        },

        Commands::Tui => tui::run().await,
    }
}

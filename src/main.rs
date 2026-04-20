use clap::Parser;
use relay_cli::cli::{
    ArtifactAction, CaptureAction, CiAction, Cli, Commands, CommitAction, ConfigAction,
    ReviewAction, SessionAction, TestAction,
};
use relay_cli::{bridge, cli, tui};
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
        Commands::Bridge {
            prompt,
            claude_model,
            claude_binary,
            gpt_model,
            reviewer_prompt_file,
            resume,
        } => {
            eprintln!(
                "warning: `relay bridge` is deprecated; use `relay chat` for the multi-agent TUI."
            );
            bridge::run(bridge::BridgeOptions {
                prompt,
                claude_model,
                claude_binary,
                gpt_model,
                reviewer_prompt_file,
                resume_session_id: resume,
            })
            .await
        }

        Commands::Chat {
            prompt,
            start_with,
            claude_model,
            claude_binary,
            codex_binary,
            gpt_model,
            system_prompt_file,
            no_auto_handoff,
            resume,
            new,
            print,
            rotation,
            format,
        } => {
            let resume_conversation_id = match resume {
                Some(s) => Some(
                    uuid::Uuid::parse_str(&s)
                        .map_err(|e| anyhow::anyhow!("invalid --resume uuid {s}: {e}"))?,
                ),
                None => None,
            };
            let harness_root = camino::Utf8PathBuf::from(".agent-harness");

            // Print mode: non-interactive, emits to stdout, then exits.
            // `--print` + `--resume` is rejected at the clap level via
            // `conflicts_with = "resume"`; resuming + print is intentionally
            // a v2 feature.
            if let Some(initial_prompt) = print {
                let start = bridge::parse_start_with(&start_with);
                let rotation_agents = match rotation {
                    Some(s) => bridge::print_mode::parse_rotation(&s)?,
                    None => vec![start],
                };
                let print_format = bridge::print_mode::PrintFormat::parse(&format)?;
                let exit_code =
                    bridge::print_mode::run_print(bridge::print_mode::PrintModeOptions {
                        initial_prompt,
                        rotation: rotation_agents,
                        format: print_format,
                        harness_dir: harness_root,
                        claude_model,
                        gpt_model: Some(gpt_model),
                        claude_binary: Some(claude_binary),
                        codex_binary: Some(codex_binary),
                        system_prompt_file,
                    })
                    .await?;
                std::process::exit(exit_code);
            }

            bridge::run_chat(bridge::ChatOptions {
                prompt,
                start_with: bridge::parse_start_with(&start_with),
                claude_model,
                claude_binary,
                codex_binary,
                gpt_model,
                system_prompt_file,
                auto_handoff: !no_auto_handoff,
                resume_conversation_id,
                skip_picker: new,
                harness_root,
            })
            .await
        }
    }
}

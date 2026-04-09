use anyhow::{bail, Context, Result};
use camino::Utf8PathBuf;
use console::style;
use uuid::Uuid;

use crate::artifacts;
use crate::ci;
use crate::commit;
use crate::config::HarnessConfig;
use crate::git;
use crate::handoff;
use crate::provider;
use crate::review;
use crate::schema::*;
use crate::session;
use crate::storage::Storage;
use crate::testing;

// ── Helpers ─────────────────────────────────────────────────────────────

fn resolve_storage() -> Result<(HarnessConfig, Storage)> {
    let cwd = std::env::current_dir().context("getting cwd")?;
    let harness_dir = cwd.join(".agent-harness");
    let harness_path =
        Utf8PathBuf::from_path_buf(harness_dir).map_err(|_| anyhow::anyhow!("non-UTF8 path"))?;
    let storage = Storage::new(harness_path.clone());

    if !storage.is_initialized() {
        bail!("Harness not initialized — run `relay init` first");
    }

    let config = HarnessConfig::load(&storage.config_path())?;
    Ok((config, storage))
}

fn print_ok(msg: &str) {
    println!("  {} {}", style("✓").green().bold(), msg);
}

fn print_warn(msg: &str) {
    println!("  {} {}", style("!").yellow().bold(), msg);
}

fn print_fail(msg: &str) {
    println!("  {} {}", style("✗").red().bold(), msg);
}

fn print_header(msg: &str) {
    println!("\n{}", style(msg).bold().underlined());
}

// ── Init ────────────────────────────────────────────────────────────────

pub async fn cmd_init() -> Result<()> {
    let cwd = std::env::current_dir().context("getting cwd")?;
    let harness_dir = cwd.join(".agent-harness");
    let harness_path =
        Utf8PathBuf::from_path_buf(harness_dir).map_err(|_| anyhow::anyhow!("non-UTF8 path"))?;

    if harness_path.as_std_path().is_dir() {
        let storage = Storage::new(harness_path.clone());
        if storage.is_initialized() {
            println!("{}", style("Harness already initialized.").yellow());
            return Ok(());
        }
    }

    let storage = Storage::new(harness_path.clone());
    storage
        .initialize()
        .context("creating storage directories")?;

    let config = HarnessConfig::default_config()?;
    config
        .save(&storage.config_path())
        .context("writing default config")?;

    println!("{}", style("Initialized relay harness").green().bold());
    println!("  Storage: {}", harness_path);
    println!("  Config:  {}/config.toml", harness_path);

    if !git::is_git_repo() {
        print_warn("Not in a git repository — some features will be limited");
    }

    println!(
        "\nNext steps:\n  \
         1. Run {} to verify your setup\n  \
         2. Run {} to start working",
        style("relay doctor").cyan(),
        style("relay session start claude").cyan()
    );

    Ok(())
}

// ── Doctor ──────────────────────────────────────────────────────────────

pub async fn cmd_doctor() -> Result<()> {
    print_header("relay doctor");

    // Git check
    print_header("Git");
    if git::is_git_repo() {
        let branch = git::current_branch()?.unwrap_or_else(|| "unknown".to_string());
        let dirty = git::is_dirty()?;
        print_ok(&format!("Git repository (branch: {branch})"));
        if dirty {
            print_warn("Working tree has uncommitted changes");
        } else {
            print_ok("Working tree clean");
        }
    } else {
        print_warn("Not in a git repository");
    }

    // Storage check
    print_header("Storage");
    let cwd = std::env::current_dir().context("getting cwd")?;
    let harness_dir = cwd.join(".agent-harness");
    let harness_path =
        Utf8PathBuf::from_path_buf(harness_dir).map_err(|_| anyhow::anyhow!("non-UTF8 path"))?;
    let storage = Storage::new(harness_path.clone());
    if storage.is_initialized() {
        print_ok(&format!("Harness initialized at {}", harness_path));
    } else {
        print_fail("Harness not initialized — run `relay init`");
    }

    // Provider checks
    print_header("Providers");
    for p in provider::all_providers() {
        println!("\n  {}", style(p.name()).bold());
        for check in p.doctor_checks() {
            if check.ok && !check.warning {
                print_ok(&check.message);
            } else if check.warning {
                print_warn(&check.message);
            } else {
                print_fail(&check.message);
            }
        }
    }

    // Dependency checks
    print_header("Dependencies");
    for dep in ["gh", "tmux"] {
        if which::which(dep).is_ok() {
            print_ok(&format!("`{}` found", dep));
        } else {
            print_warn(&format!("`{}` not found (optional)", dep));
        }
    }

    println!();
    Ok(())
}

// ── Session ─────────────────────────────────────────────────────────────

pub async fn cmd_session_start(provider_name: &str, model: Option<String>) -> Result<()> {
    let (config, storage) = resolve_storage()?;

    let binary_override = config
        .provider_config(provider_name)
        .and_then(|c| c.binary.as_deref());
    let p = provider::get_provider(provider_name, binary_override)
        .with_context(|| format!("unknown provider: {provider_name}"))?;

    let install_check = p.validate_installation();
    if !install_check.ok {
        bail!("{}", install_check.message);
    }

    let cmd = p.build_launch_command(model.as_deref())?;
    println!(
        "{} Starting {} session...",
        style("▶").cyan().bold(),
        style(provider_name).bold()
    );

    let record = session::start_session(
        &storage,
        provider_name,
        SessionRole::InteractivePrimary,
        model,
        cmd,
    )
    .await?;

    println!(
        "\n{} Session {} finished ({:?})",
        style("■").cyan().bold(),
        &record.id.to_string()[..8],
        record.status
    );

    Ok(())
}

pub async fn cmd_session_list() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let sessions = session::list_sessions(&storage)?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!(
        "{:<10} {:<12} {:<20} {:<12} {}",
        "ID", "PROVIDER", "ROLE", "STATUS", "STARTED"
    );
    println!("{}", "-".repeat(70));
    for s in &sessions {
        println!(
            "{:<10} {:<12} {:<20} {:<12} {}",
            &s.id.to_string()[..8],
            s.provider,
            format!("{:?}", s.role),
            format!("{:?}", s.status),
            s.started_at.format("%Y-%m-%d %H:%M")
        );
    }

    Ok(())
}

pub async fn cmd_session_stop(id: &str) -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let uuid = Uuid::parse_str(id).context("invalid session ID")?;
    let record = session::stop_session(&storage, uuid)?;
    println!("Session {} stopped.", &record.id.to_string()[..8]);
    Ok(())
}

pub async fn cmd_session_show(id: &str) -> Result<()> {
    let (_config, storage) = resolve_storage()?;

    let record = if id == "latest" {
        session::latest_session(&storage)?.context("no sessions found")?
    } else {
        let uuid = Uuid::parse_str(id).context("invalid session ID")?;
        session::load_session(&storage, uuid)?
    };

    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

// ── Capture ─────────────────────────────────────────────────────────────

pub async fn cmd_capture_last_response(
    session_id: Option<String>,
    file: Option<String>,
) -> Result<()> {
    let (_config, storage) = resolve_storage()?;

    let content = if let Some(path) = file {
        std::fs::read_to_string(&path).with_context(|| format!("reading file {path}"))?
    } else {
        // Try to read from latest session's stdout log.
        let record = if let Some(id) = session_id {
            let uuid = Uuid::parse_str(&id).context("invalid session ID")?;
            session::load_session(&storage, uuid)?
        } else {
            session::latest_session(&storage)?.context("no sessions found")?
        };

        let log_path = storage.session_stdout_path(record.id);
        if log_path.as_std_path().is_file() {
            std::fs::read_to_string(log_path.as_std_path()).context("reading stdout log")?
        } else {
            bail!(
                "No stdout log found for session {}. Use --file to specify a file to capture.",
                &record.id.to_string()[..8]
            );
        }
    };

    let sess = session::latest_session(&storage)?;
    let session_id = sess.map(|s| s.id).unwrap_or_else(Uuid::new_v4);
    let git_ctx = git::collect_context().ok();

    let manifest = artifacts::save_artifact(
        &storage,
        session_id,
        ArtifactType::LastResponse,
        &content,
        git_ctx,
    )?;

    println!(
        "{} Captured last response as artifact {}",
        style("✓").green().bold(),
        &manifest.id.to_string()[..8]
    );
    println!("  Path: {}", manifest.path);

    Ok(())
}

pub async fn cmd_capture_transcript(
    session_id: Option<String>,
    file: Option<String>,
) -> Result<()> {
    let (_config, storage) = resolve_storage()?;

    let content = if let Some(path) = file {
        std::fs::read_to_string(&path).with_context(|| format!("reading file {path}"))?
    } else {
        let record = if let Some(id) = session_id {
            let uuid = Uuid::parse_str(&id).context("invalid session ID")?;
            session::load_session(&storage, uuid)?
        } else {
            session::latest_session(&storage)?.context("no sessions found")?
        };

        let conv_path = storage.session_conversation_path(record.id);
        if conv_path.as_std_path().is_file() {
            std::fs::read_to_string(conv_path.as_std_path()).context("reading conversation log")?
        } else {
            bail!(
                "No conversation log found for session {}. Use --file to specify a file.",
                &record.id.to_string()[..8]
            );
        }
    };

    let sess = session::latest_session(&storage)?;
    let session_id = sess.map(|s| s.id).unwrap_or_else(Uuid::new_v4);
    let git_ctx = git::collect_context().ok();

    let manifest = artifacts::save_artifact(
        &storage,
        session_id,
        ArtifactType::FullConversation,
        &content,
        git_ctx,
    )?;

    println!(
        "{} Captured transcript as artifact {}",
        style("✓").green().bold(),
        &manifest.id.to_string()[..8]
    );
    println!("  Path: {}", manifest.path);

    Ok(())
}

// ── Review ──────────────────────────────────────────────────────────────

pub async fn cmd_review_codex(
    artifact_id: Option<String>,
    model: Option<String>,
    goal: String,
) -> Result<()> {
    let (config, storage) = resolve_storage()?;

    // Get the content to review.
    let (artifact_manifest, content) = if let Some(id) = artifact_id {
        let uuid = Uuid::parse_str(&id).context("invalid artifact ID")?;
        artifacts::read_artifact(&storage, uuid)?
    } else {
        // Use the latest artifact.
        let all = artifacts::list_artifacts(&storage)?;
        let latest = all
            .into_iter()
            .find(|a| {
                matches!(
                    a.artifact_type,
                    ArtifactType::LastResponse | ArtifactType::FullConversation
                )
            })
            .context("no capture artifacts found — run `relay capture last-response` first")?;
        let content = std::fs::read_to_string(latest.path.as_std_path())
            .context("reading artifact content")?;
        (latest, content)
    };

    // Get codex provider.
    let binary_override = config
        .provider_config("codex")
        .and_then(|c| c.binary.as_deref());
    let p = provider::get_provider("codex", binary_override).context("codex provider not found")?;

    let install_check = p.validate_installation();
    if !install_check.ok {
        bail!("{}", install_check.message);
    }

    // Create handoff.
    let git_ctx = git::collect_context().ok();
    let handoff_manifest = handoff::create_handoff(
        &storage,
        Some(artifact_manifest.session_id),
        "codex",
        SessionRole::Reviewer,
        &goal,
        HandoffScope::LastResponse,
        vec![artifact_manifest.path.clone()],
        git_ctx,
        model.clone(),
        SafetyMode::ReadOnly,
    )?;

    println!(
        "{} Sending to Codex for review...",
        style("→").cyan().bold()
    );

    // Run review.
    let result = review::run_review(&storage, p.as_ref(), &handoff_manifest, &content).await?;

    // Print results.
    println!();
    println!(
        "{} Review complete — verdict: {}",
        style("←").cyan().bold(),
        match result.verdict {
            Verdict::Pass => style("PASS").green().bold().to_string(),
            Verdict::Fail => style("FAIL").red().bold().to_string(),
            Verdict::NeedsWork => style("NEEDS WORK").yellow().bold().to_string(),
            Verdict::Inconclusive => style("INCONCLUSIVE").dim().to_string(),
        }
    );
    println!();
    println!("{}", result.summary);

    if !result.findings.is_empty() {
        println!("\nFindings ({}):", result.findings.len());
        for (i, f) in result.findings.iter().enumerate() {
            let severity_styled = match f.severity.as_str() {
                "critical" | "high" => style(&f.severity).red().bold().to_string(),
                "medium" => style(&f.severity).yellow().to_string(),
                _ => style(&f.severity).dim().to_string(),
            };
            println!(
                "  {}. [{}] {} — {}",
                i + 1,
                severity_styled,
                f.category,
                f.message
            );
        }
    }

    println!(
        "\n  Report: {}",
        storage.handoff_result_md_path(handoff_manifest.id)
    );

    Ok(())
}

pub async fn cmd_review_history() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let handoffs = handoff::list_handoffs(&storage)?;

    let reviews: Vec<_> = handoffs
        .iter()
        .filter(|h| h.target_role == SessionRole::Reviewer)
        .collect();

    if reviews.is_empty() {
        println!("No reviews found.");
        return Ok(());
    }

    println!("{:<10} {:<12} {:<12} {}", "ID", "PROVIDER", "DATE", "GOAL");
    println!("{}", "-".repeat(60));
    for h in &reviews {
        println!(
            "{:<10} {:<12} {:<12} {}",
            &h.id.to_string()[..8],
            h.target_provider,
            h.created_at.format("%Y-%m-%d"),
            &h.goal[..h.goal.len().min(40)]
        );
    }

    Ok(())
}

// ── Test ────────────────────────────────────────────────────────────────

pub async fn cmd_test_run(commands: Vec<String>) -> Result<()> {
    let (config, storage) = resolve_storage()?;

    let cmds = if commands.is_empty() {
        // Use configured test commands.
        config
            .role_config("tester")
            .map(|r| r.test_commands.clone())
            .unwrap_or_default()
    } else {
        commands
    };

    if cmds.is_empty() {
        bail!(
            "No test commands configured. Either pass --command or set roles.tester.test_commands in config."
        );
    }

    println!(
        "{} Running {} test command(s)...",
        style("▶").cyan().bold(),
        cmds.len()
    );

    let result = testing::run_tests(&storage, &cmds, None).await?;

    println!();
    for r in &result.commands_run {
        let status = if r.exit_code == 0 {
            style("PASS").green().bold()
        } else {
            style("FAIL").red().bold()
        };
        println!("  {} `{}` ({:.1}s)", status, r.command, r.duration_secs);
    }

    println!(
        "\n{} Verdict: {:?}",
        if result.verdict == Verdict::Pass {
            style("✓").green().bold()
        } else {
            style("✗").red().bold()
        },
        result.verdict
    );

    Ok(())
}

pub async fn cmd_test_history() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let all = artifacts::list_artifacts(&storage)?;
    let tests: Vec<_> = all
        .iter()
        .filter(|a| a.artifact_type == ArtifactType::TestReport)
        .collect();

    if tests.is_empty() {
        println!("No test results found.");
        return Ok(());
    }

    for t in &tests {
        println!(
            "  {} — {} ({})",
            &t.id.to_string()[..8],
            t.created_at.format("%Y-%m-%d %H:%M"),
            t.path
        );
    }

    Ok(())
}

// ── Commit ──────────────────────────────────────────────────────────────

pub async fn cmd_commit_prepare() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let proposal = commit::prepare_commit(&storage)?;

    println!("{}", style("Commit Proposal").bold().underlined());
    println!("\nFiles changed:");
    for f in &proposal.files_changed {
        println!("  {}", f);
    }
    println!("\nDiff stat:\n{}", proposal.diff_stat);
    println!(
        "\n  Proposal saved: {}",
        storage
            .artifacts_dir()
            .join(proposal.id.to_string())
            .join("commit-proposal.json")
    );

    Ok(())
}

// ── CI ──────────────────────────────────────────────────────────────────

pub async fn cmd_ci_watch() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    println!("{} Checking CI status...", style("◎").cyan().bold());

    let snapshot = ci::check_ci_status(&storage).await?;

    let status_styled = match snapshot.status.as_str() {
        "passing" => style(&snapshot.status).green().bold().to_string(),
        "failing" => style(&snapshot.status).red().bold().to_string(),
        "running" => style(&snapshot.status).yellow().bold().to_string(),
        _ => style(&snapshot.status).dim().to_string(),
    };

    println!("\n  Status: {}", status_styled);

    if !snapshot.failed_jobs.is_empty() {
        println!("\n  Failed jobs:");
        for j in &snapshot.failed_jobs {
            println!("    {} {}", style("✗").red(), j);
        }
    }

    Ok(())
}

// ── E2E ─────────────────────────────────────────────────────────────────

pub async fn cmd_e2e(commands: Vec<String>) -> Result<()> {
    let (_config, storage) = resolve_storage()?;

    if commands.is_empty() {
        bail!("No E2E commands specified. Use --command to provide them.");
    }

    println!(
        "{} Running {} E2E command(s)...",
        style("▶").cyan().bold(),
        commands.len()
    );

    let result = crate::e2e::run_e2e(&storage, &commands).await?;

    for r in &result.commands_run {
        let status = if r.exit_code == 0 {
            style("PASS").green().bold()
        } else {
            style("FAIL").red().bold()
        };
        println!("  {} `{}` ({:.1}s)", status, r.command, r.duration_secs);
    }

    println!("\nVerdict: {:?}", result.verdict);
    Ok(())
}

// ── Artifacts ───────────────────────────────────────────────────────────

pub async fn cmd_artifacts_list() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let all = artifacts::list_artifacts(&storage)?;

    if all.is_empty() {
        println!("No artifacts found.");
        return Ok(());
    }

    println!("{:<10} {:<18} {:<22} {}", "ID", "TYPE", "CREATED", "PATH");
    println!("{}", "-".repeat(80));
    for a in &all {
        println!(
            "{:<10} {:<18} {:<22} {}",
            &a.id.to_string()[..8],
            format!("{:?}", a.artifact_type),
            a.created_at.format("%Y-%m-%d %H:%M"),
            a.path
        );
    }

    Ok(())
}

pub async fn cmd_artifacts_show(id: &str) -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let uuid = Uuid::parse_str(id).context("invalid artifact ID")?;
    let (manifest, content) = artifacts::read_artifact(&storage, uuid)?;

    println!(
        "{} Artifact {} ({:?})",
        style("◆").cyan(),
        &manifest.id.to_string()[..8],
        manifest.artifact_type
    );
    println!(
        "Created: {}",
        manifest.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("Path: {}", manifest.path);
    if let Some(ref ctx) = manifest.git_context {
        println!(
            "Git: branch={} sha={} dirty={}",
            ctx.branch.as_deref().unwrap_or("?"),
            ctx.commit_sha.as_deref().map(|s| &s[..8]).unwrap_or("?"),
            ctx.is_dirty
        );
    }
    println!("\n{}", "-".repeat(60));
    println!("{}", content);

    Ok(())
}

// ── Config ──────────────────────────────────────────────────────────────

pub async fn cmd_config_show() -> Result<()> {
    let (config, _storage) = resolve_storage()?;
    println!("{}", toml::to_string_pretty(&config)?);
    Ok(())
}

pub async fn cmd_config_edit() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let path = storage.config_path();
    let status = std::process::Command::new(&editor)
        .arg(path.as_str())
        .status()
        .with_context(|| format!("launching editor `{editor}`"))?;
    if !status.success() {
        bail!("Editor exited with non-zero status");
    }
    Ok(())
}

pub async fn cmd_config_reset() -> Result<()> {
    let (_config, storage) = resolve_storage()?;
    let config = HarnessConfig::default_config()?;
    config.save(&storage.config_path())?;
    println!("{} Config reset to defaults.", style("✓").green().bold());
    Ok(())
}

// ── Logs ────────────────────────────────────────────────────────────────

pub async fn cmd_logs(session_id: Option<String>, lines: usize) -> Result<()> {
    let (_config, storage) = resolve_storage()?;

    let record = if let Some(id) = session_id {
        let uuid = Uuid::parse_str(&id).context("invalid session ID")?;
        session::load_session(&storage, uuid)?
    } else {
        session::latest_session(&storage)?.context("no sessions found")?
    };

    let stdout_path = storage.session_stdout_path(record.id);
    if stdout_path.as_std_path().is_file() {
        let content = std::fs::read_to_string(stdout_path.as_std_path())?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(lines);
        for line in &all_lines[start..] {
            println!("{}", line);
        }
    } else {
        println!("No logs found for session {}.", &record.id.to_string()[..8]);
    }

    Ok(())
}

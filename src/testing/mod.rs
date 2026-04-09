use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::process;
use crate::schema::{TestCommandResult, TestResult, Verdict, SCHEMA_VERSION};
use crate::storage::Storage;

/// Run test commands and collect results.
pub async fn run_tests(
    storage: &Storage,
    commands: &[String],
    session_id: Option<Uuid>,
) -> Result<TestResult> {
    let mut results = Vec::new();
    let mut failures = Vec::new();

    for cmd_str in commands {
        let args: Vec<String> = shell_words::split(cmd_str)
            .unwrap_or_else(|_| vec!["sh".to_string(), "-c".to_string(), cmd_str.clone()]);

        let start = std::time::Instant::now();
        let proc_result = process::run_capture(&args, None).await?;
        let duration = start.elapsed().as_secs_f64();

        if proc_result.exit_code != 0 {
            failures.push(format!(
                "Command `{}` failed with exit code {}",
                cmd_str, proc_result.exit_code
            ));
        }

        results.push(TestCommandResult {
            command: cmd_str.clone(),
            exit_code: proc_result.exit_code,
            stdout: proc_result.stdout,
            stderr: proc_result.stderr,
            duration_secs: duration,
        });
    }

    let verdict = if failures.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail
    };

    let test_result = TestResult {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4(),
        session_id,
        created_at: Utc::now(),
        commands_run: results,
        failures,
        verdict,
    };

    // Save result.
    let result_dir = storage.artifacts_dir().join(test_result.id.to_string());
    std::fs::create_dir_all(result_dir.as_std_path()).context("creating test result dir")?;
    let result_path = result_dir.join("test-result.json");
    let json = serde_json::to_string_pretty(&test_result).context("serializing test result")?;
    std::fs::write(result_path.as_std_path(), json).context("writing test result")?;

    Ok(test_result)
}

/// Format test results as markdown.
pub fn format_test_markdown(result: &TestResult) -> String {
    let mut md = String::new();
    md.push_str("# Test Report\n\n");
    md.push_str(&format!(
        "**Date:** {}\n",
        result.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    md.push_str(&format!("**Verdict:** {:?}\n\n", result.verdict));

    md.push_str("## Commands\n\n");
    for r in &result.commands_run {
        let status = if r.exit_code == 0 { "PASS" } else { "FAIL" };
        md.push_str(&format!("### `{}` — {}\n\n", r.command, status));
        md.push_str(&format!(
            "Exit code: {} | Duration: {:.1}s\n\n",
            r.exit_code, r.duration_secs
        ));
        if !r.stdout.is_empty() {
            md.push_str("<details><summary>stdout</summary>\n\n```\n");
            md.push_str(&r.stdout);
            md.push_str("```\n\n</details>\n\n");
        }
        if !r.stderr.is_empty() {
            md.push_str("<details><summary>stderr</summary>\n\n```\n");
            md.push_str(&r.stderr);
            md.push_str("```\n\n</details>\n\n");
        }
    }

    if !result.failures.is_empty() {
        md.push_str("## Failures\n\n");
        for f in &result.failures {
            md.push_str(&format!("- {}\n", f));
        }
    }

    md
}

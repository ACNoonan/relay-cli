use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::handoff;
use crate::process;
use crate::provider::Provider;
use crate::schema::{HandoffManifest, ReviewFinding, ReviewResult, Verdict, SCHEMA_VERSION};
use crate::storage::Storage;

/// Run a review via a provider and parse structured results.
pub async fn run_review(
    storage: &Storage,
    provider: &dyn Provider,
    handoff_manifest: &HandoffManifest,
    content: &str,
) -> Result<ReviewResult> {
    let prompt = handoff::build_review_prompt(handoff_manifest, content);

    // Save the prompt.
    let prompt_path = storage.handoff_prompt_path(handoff_manifest.id);
    std::fs::write(prompt_path.as_std_path(), &prompt).context("writing review prompt")?;

    // Build and run the review command.
    let model = handoff_manifest.model_override.as_deref();
    let cmd = provider.build_prompt_command(&prompt, model)?;
    let result = process::run_capture(&cmd, None).await?;

    // Try to parse structured JSON from output.
    let review = parse_review_output(&result.stdout, handoff_manifest.id, provider.name(), model);

    // Save raw result.
    let result_path = storage.handoff_result_json_path(handoff_manifest.id);
    let result_json = serde_json::to_string_pretty(&review).context("serializing review")?;
    std::fs::write(result_path.as_std_path(), &result_json).context("writing review JSON")?;

    // Save markdown report.
    let md_report = format_review_markdown(&review);
    let md_path = storage.handoff_result_md_path(handoff_manifest.id);
    std::fs::write(md_path.as_std_path(), &md_report).context("writing review markdown")?;

    Ok(review)
}

/// Parse the provider's output into a ReviewResult.
fn parse_review_output(
    stdout: &str,
    handoff_id: Uuid,
    provider_name: &str,
    model: Option<&str>,
) -> ReviewResult {
    // Try to extract JSON from the output (may be wrapped in ```json blocks).
    let json_str = extract_json_block(stdout);

    if let Some(json) = json_str {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
            let summary = parsed["summary"]
                .as_str()
                .unwrap_or("No summary provided")
                .to_string();

            let findings: Vec<ReviewFinding> = parsed["findings"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| serde_json::from_value(f.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();

            let verdict = match parsed["verdict"].as_str() {
                Some("pass") => Verdict::Pass,
                Some("fail") => Verdict::Fail,
                Some("needs_work") => Verdict::NeedsWork,
                _ => Verdict::Inconclusive,
            };

            return ReviewResult {
                schema_version: SCHEMA_VERSION,
                id: Uuid::new_v4(),
                handoff_id,
                provider: provider_name.to_string(),
                model: model.map(|s| s.to_string()),
                created_at: Utc::now(),
                summary,
                findings,
                verdict,
                raw_output: Some(stdout.to_string()),
            };
        }
    }

    // Fallback: couldn't parse structured output.
    ReviewResult {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4(),
        handoff_id,
        provider: provider_name.to_string(),
        model: model.map(|s| s.to_string()),
        created_at: Utc::now(),
        summary: "Could not parse structured review output".to_string(),
        findings: vec![],
        verdict: Verdict::Inconclusive,
        raw_output: Some(stdout.to_string()),
    }
}

/// Extract a JSON block from output that may contain ```json fences.
fn extract_json_block(text: &str) -> Option<String> {
    // Try ```json ... ``` first.
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // Try finding raw JSON object.
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

/// Format a ReviewResult as a markdown report.
pub fn format_review_markdown(review: &ReviewResult) -> String {
    let mut md = String::new();
    md.push_str("# Review Report\n\n");
    md.push_str(&format!("**Provider:** {}\n", review.provider));
    if let Some(ref m) = review.model {
        md.push_str(&format!("**Model:** {}\n", m));
    }
    md.push_str(&format!(
        "**Date:** {}\n",
        review.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    md.push_str(&format!("**Verdict:** {:?}\n\n", review.verdict));
    md.push_str(&format!("## Summary\n\n{}\n\n", review.summary));

    if !review.findings.is_empty() {
        md.push_str("## Findings\n\n");
        for (i, f) in review.findings.iter().enumerate() {
            md.push_str(&format!(
                "### {}. [{}] {}\n\n",
                i + 1,
                f.severity.to_uppercase(),
                f.category
            ));
            if let Some(ref file) = f.file {
                md.push_str(&format!("**File:** {}", file));
                if let Some(line) = f.line {
                    md.push_str(&format!(":{}", line));
                }
                md.push('\n');
            }
            md.push_str(&format!("\n{}\n", f.message));
            if let Some(ref suggestion) = f.suggestion {
                md.push_str(&format!("\n**Suggestion:** {}\n", suggestion));
            }
            md.push('\n');
        }
    } else {
        md.push_str("## Findings\n\nNo findings.\n\n");
    }

    md
}

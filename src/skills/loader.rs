//! Skill file format + on-disk discovery.
//!
//! # File format
//!
//! ```markdown
//! ---
//! name: security-review
//! description: Claude implements, Codex security-reviews, GPT summarizes risks
//! rotation: [claude, codex, gpt]
//! prompts:
//!   codex: "Focus on injection, auth bypass, and data leakage. Cite line:col."
//!   gpt: "Summarize Codex's findings in 3 bullets ranked by severity."
//! ---
//!
//! # Body (optional)
//!
//! Anything after the closing `---` is treated as additional context that is
//! prepended to the **first** agent's prompt when the skill runs. Useful for
//! checklists, persona boilerplate, or skill-wide instructions.
//! ```
//!
//! Frontmatter schema:
//! - `name` (string, required, ≤64 chars, must match `^[a-z][a-z0-9-]*$`).
//! - `description` (string, required, ≤1024 chars).
//! - `rotation` (array of agent names; required; each ∈ {claude, gpt, codex}).
//! - `prompts` (map<agent_name, string>, optional). When the rotation reaches
//!   an agent listed here, the agent receives `<previous-assistant-response>\n\n---\n\n<this prompt>`
//!   instead of the bare handoff template — the per-agent prompt is appended
//!   after the standard handoff body.
//!
//! # YAML
//!
//! Frontmatter is small and constrained, so we parse it by hand rather than
//! pull in `serde_yaml`/`gray_matter`. Supported subset: top-level scalar
//! strings, `[a, b, c]` flow-style arrays, and `key: value` indented blocks
//! for `prompts`. Quoted (single or double) and bare scalars both work; multi-
//! line block scalars (`|`, `>`) are not supported. Anything outside this
//! subset produces a per-file error rather than a panic.

use std::collections::{HashMap, HashSet};

use camino::{Utf8Path, Utf8PathBuf};

use crate::bridge::conversation::Agent;

/// Maximum length of a skill `name` (matches pi-mono spec).
pub const MAX_NAME_LEN: usize = 64;

/// Maximum length of a skill `description` (matches pi-mono spec).
pub const MAX_DESC_LEN: usize = 1024;

/// Where a loaded skill came from. Used to resolve name collisions
/// (project wins over global) and to label entries in `/skills` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// Loaded from `~/.config/relay/skills/`.
    Global,
    /// Loaded from `<harness>/skills/`.
    Project,
}

impl SkillScope {
    pub fn label(self) -> &'static str {
        match self {
            SkillScope::Global => "global",
            SkillScope::Project => "project",
        }
    }
}

/// A successfully loaded skill, ready to execute.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub rotation: Vec<Agent>,
    pub per_agent_prompts: HashMap<Agent, String>,
    /// Markdown body with the frontmatter block stripped, trimmed of leading/
    /// trailing whitespace. Empty when the file has no body.
    pub body: String,
    pub source_path: Utf8PathBuf,
    pub scope: SkillScope,
}

/// Raw frontmatter shape, before validation. Public so tests + future
/// alternative loaders can build skills programmatically.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub rotation: Option<Vec<String>>,
    pub prompts: HashMap<String, String>,
}

/// One per-file diagnostic. The loader collects these instead of returning
/// `Err` so a single bad skill cannot break chat startup.
#[derive(Debug, Clone)]
pub struct SkillError {
    pub path: Utf8PathBuf,
    pub message: String,
}

impl SkillError {
    fn new(path: impl Into<Utf8PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Public entry points
// ───────────────────────────────────────────────────────────────────────────

/// Discover and parse `.md` files directly under `dir` as skills, tagging each
/// with `scope`. Returns `(skills, errors)`. A non-existent or unreadable
/// directory returns empty results, no errors.
///
/// Within a single directory, name collisions are reported as errors (only
/// the first occurrence is kept). Cross-scope collisions are resolved by
/// [`SkillRegistry::load`](super::SkillRegistry::load), not here.
pub fn load_skills_from_dir(dir: &Utf8Path, scope: SkillScope) -> (Vec<Skill>, Vec<SkillError>) {
    let mut skills = Vec::new();
    let mut errors = Vec::new();

    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return (skills, errors), // not initialised, that's fine
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut entries: Vec<Utf8PathBuf> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let utf8 = match Utf8PathBuf::try_from(path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if utf8.extension() != Some("md") {
            continue;
        }
        // Skip directories that happen to end in `.md` (rare but possible).
        if utf8.is_dir() {
            continue;
        }
        entries.push(utf8);
    }
    // Deterministic order for tests + collision messages.
    entries.sort();

    for path in entries {
        match parse_skill_file(&path, scope) {
            Ok(skill) => {
                if !seen.insert(skill.name.clone()) {
                    errors.push(SkillError::new(
                        path.clone(),
                        format!(
                            "duplicate skill name {:?} in {} scope (kept earlier file)",
                            skill.name,
                            scope.label()
                        ),
                    ));
                    continue;
                }
                skills.push(skill);
            }
            Err(e) => errors.push(e),
        }
    }

    (skills, errors)
}

/// Read + parse a single skill file. Public for unit testing the loader and
/// for callers that want to hand-load a known file (e.g. a CLI `--skill <path>`
/// flag we may add later).
pub fn parse_skill_file(path: &Utf8Path, scope: SkillScope) -> Result<Skill, SkillError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| SkillError::new(path, format!("read failed: {e}")))?;
    parse_skill_content(&content, path, scope)
}

/// Pure parse step exposed for unit tests; takes the raw file bytes and the
/// path to use for error attribution.
pub fn parse_skill_content(
    content: &str,
    path: &Utf8Path,
    scope: SkillScope,
) -> Result<Skill, SkillError> {
    let (yaml, body) = split_frontmatter(content).ok_or_else(|| {
        SkillError::new(
            path,
            "missing or unterminated YAML frontmatter (`---` block)",
        )
    })?;
    let frontmatter = parse_frontmatter(yaml).map_err(|e| SkillError::new(path, e))?;

    let name = frontmatter
        .name
        .as_deref()
        .ok_or_else(|| SkillError::new(path, "missing required field `name`"))?;
    validate_name(name).map_err(|e| SkillError::new(path, e))?;

    let description = frontmatter
        .description
        .as_deref()
        .ok_or_else(|| SkillError::new(path, "missing required field `description`"))?;
    if description.trim().is_empty() {
        return Err(SkillError::new(path, "`description` must not be empty"));
    }
    if description.len() > MAX_DESC_LEN {
        return Err(SkillError::new(
            path,
            format!(
                "`description` exceeds {MAX_DESC_LEN} chars ({})",
                description.len()
            ),
        ));
    }

    let rotation_raw = frontmatter
        .rotation
        .as_deref()
        .ok_or_else(|| SkillError::new(path, "missing required field `rotation`"))?;
    if rotation_raw.is_empty() {
        return Err(SkillError::new(
            path,
            "`rotation` must list at least one agent",
        ));
    }
    let mut rotation = Vec::with_capacity(rotation_raw.len());
    for raw in rotation_raw {
        let agent =
            parse_agent_name(raw).map_err(|e| SkillError::new(path, format!("rotation: {e}")))?;
        rotation.push(agent);
    }

    let mut per_agent_prompts: HashMap<Agent, String> = HashMap::new();
    for (k, v) in frontmatter.prompts {
        let agent =
            parse_agent_name(&k).map_err(|e| SkillError::new(path, format!("prompts.{k}: {e}")))?;
        per_agent_prompts.insert(agent, v);
    }

    Ok(Skill {
        name: name.to_string(),
        description: description.to_string(),
        rotation,
        per_agent_prompts,
        body: body.trim().to_string(),
        source_path: path.to_path_buf(),
        scope,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Frontmatter splitter
// ───────────────────────────────────────────────────────────────────────────

/// Returns `Some((yaml_block, body))` when `content` opens with a `---` line
/// and contains a closing `---` line. CRLF is normalised to LF before search.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    // Accept BOM + CRLF, but parse in terms of the raw content so byte ranges
    // stay valid. We only need to spot the opening + closing `---` markers.
    let trimmed = content.strip_prefix('\u{feff}').unwrap_or(content);
    // Must start with `---` followed by newline.
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))?;
    // Find the closing line. We look for `\n---` followed by newline OR EOF.
    // To allow CRLF, replace search target accordingly.
    let bytes = after_open.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Match a line that is exactly `---` (optionally with trailing `\r`).
        if (i == 0 || bytes[i - 1] == b'\n') && bytes.len() >= i + 3 && &bytes[i..i + 3] == b"---" {
            let after = i + 3;
            // Must be EOL or EOF.
            if after == bytes.len() || bytes[after] == b'\n' || bytes[after] == b'\r' {
                let yaml = &after_open[..i];
                // Strip leading blank line + the marker; body starts after the
                // marker's newline.
                let mut body_start = after;
                if body_start < bytes.len() && bytes[body_start] == b'\r' {
                    body_start += 1;
                }
                if body_start < bytes.len() && bytes[body_start] == b'\n' {
                    body_start += 1;
                }
                return Some((yaml, &after_open[body_start..]));
            }
        }
        i += 1;
    }
    None
}

// ───────────────────────────────────────────────────────────────────────────
// Hand-rolled YAML subset parser
// ───────────────────────────────────────────────────────────────────────────

/// Parse the limited YAML shape we accept for skill frontmatter. Supported:
///
/// * `key: value` scalar (bare, single-, or double-quoted).
/// * `key: [a, b, c]` flow-style arrays (scalars only).
/// * Nested map `prompts:` followed by indented `agent: "text"` lines.
///
/// Unsupported (returns Err): block scalars (`|`, `>`), nested arrays,
/// anchors/aliases, multi-document streams.
fn parse_frontmatter(yaml: &str) -> Result<SkillFrontmatter, String> {
    let mut fm = SkillFrontmatter::default();
    let mut iter = yaml.lines().enumerate().peekable();

    while let Some((lineno, raw)) = iter.next() {
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }

        // Top-level key must start at column 0 (no leading whitespace).
        if line.starts_with(|c: char| c.is_whitespace()) {
            return Err(format!(
                "line {}: unexpected indented line at top level: {raw:?}",
                lineno + 1
            ));
        }

        let (key, rest) = match line.find(':') {
            Some(idx) => (&line[..idx], line[idx + 1..].trim_end()),
            None => {
                return Err(format!(
                    "line {}: expected `key: value`: {raw:?}",
                    lineno + 1
                ))
            }
        };
        let key = key.trim();
        let value = rest.trim_start();

        match key {
            "name" => {
                fm.name = Some(
                    parse_scalar(value).map_err(|e| format!("line {}: name: {e}", lineno + 1))?,
                );
            }
            "description" => {
                fm.description = Some(
                    parse_scalar(value)
                        .map_err(|e| format!("line {}: description: {e}", lineno + 1))?,
                );
            }
            "rotation" => {
                if value.is_empty() {
                    return Err(format!(
                        "line {}: rotation must use flow syntax `[a, b, c]`",
                        lineno + 1
                    ));
                }
                let arr = parse_flow_array(value)
                    .map_err(|e| format!("line {}: rotation: {e}", lineno + 1))?;
                fm.rotation = Some(arr);
            }
            "prompts" => {
                if !value.is_empty() {
                    return Err(format!(
                        "line {}: prompts must be a block map, not inline",
                        lineno + 1
                    ));
                }
                // Drain following indented lines as `subkey: value`.
                while let Some((_lineno, peek)) = iter.peek() {
                    if peek.trim().is_empty() {
                        iter.next();
                        continue;
                    }
                    if !peek.starts_with(|c: char| c == ' ' || c == '\t') {
                        break;
                    }
                    let (sub_lineno, sub_raw) = iter.next().expect("peeked");
                    let sub_line = strip_comment(sub_raw);
                    let inner = sub_line.trim_start();
                    let (sk, sv) = inner.split_once(':').ok_or_else(|| {
                        format!(
                            "line {}: expected `agent: \"text\"` under prompts",
                            sub_lineno + 1
                        )
                    })?;
                    let sk = sk.trim().to_string();
                    let sv = parse_scalar(sv.trim())
                        .map_err(|e| format!("line {}: prompts.{sk}: {e}", sub_lineno + 1))?;
                    fm.prompts.insert(sk, sv);
                }
            }
            other => {
                return Err(format!(
                    "line {}: unknown field {other:?} (allowed: name, description, rotation, prompts)",
                    lineno + 1
                ));
            }
        }
    }

    Ok(fm)
}

/// Strip an unquoted `# comment` tail. Doesn't try to be clever about `#`
/// inside quoted strings — the only quoted strings we accept are scalar
/// values, and `parse_scalar` runs after this strip on the remaining text.
fn strip_comment(line: &str) -> &str {
    // Only strip if `#` is preceded by whitespace OR is the first char,
    // so URLs like `https://#frag` aren't broken in scalars (still imperfect
    // but good enough for our shape).
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return &line[..i];
        }
    }
    line
}

/// Parse a single YAML scalar value: bare token, `'single'`, or `"double"`
/// quoted. Quoted values support `\\`, `\"`, `\n`, `\t` minimally.
fn parse_scalar(value: &str) -> Result<String, String> {
    let v = value.trim();
    if v.is_empty() {
        return Err("expected a value".to_string());
    }
    if let Some(rest) = v.strip_prefix('"') {
        let inner = rest
            .strip_suffix('"')
            .ok_or("unterminated double-quoted string")?;
        return Ok(unescape_double(inner));
    }
    if let Some(rest) = v.strip_prefix('\'') {
        let inner = rest
            .strip_suffix('\'')
            .ok_or("unterminated single-quoted string")?;
        return Ok(inner.replace("''", "'"));
    }
    Ok(v.to_string())
}

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse `[a, "b", 'c']` into `vec!["a", "b", "c"]`.
fn parse_flow_array(value: &str) -> Result<Vec<String>, String> {
    let v = value.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or("expected `[a, b, c]` flow syntax")?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Split on commas at the top level (no nesting expected).
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_double = false;
    let mut in_single = false;
    for ch in inner.chars() {
        match ch {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            ',' if !in_double && !in_single => {
                out.push(parse_scalar(buf.trim())?);
                buf.clear();
                continue;
            }
            _ => {}
        }
        buf.push(ch);
    }
    let last = buf.trim();
    if !last.is_empty() {
        out.push(parse_scalar(last)?);
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Validation
// ───────────────────────────────────────────────────────────────────────────

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("`name` must not be empty".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "`name` exceeds {MAX_NAME_LEN} chars ({})",
            name.len()
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() {
        return Err(format!(
            "`name` must start with a lowercase letter (got {first:?})"
        ));
    }
    for c in chars {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !ok {
            return Err(format!(
                "`name` may only contain lowercase letters, digits, and hyphens (got {c:?})"
            ));
        }
    }
    if name.ends_with('-') {
        return Err("`name` must not end with a hyphen".to_string());
    }
    Ok(())
}

fn parse_agent_name(raw: &str) -> Result<Agent, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Ok(Agent::Claude),
        "gpt" => Ok(Agent::Gpt),
        "codex" => Ok(Agent::Codex),
        other => Err(format!(
            "unknown agent {other:?} (allowed: claude, gpt, codex)"
        )),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Utf8PathBuf {
        Utf8PathBuf::from(s)
    }

    #[test]
    fn parses_well_formed_skill() {
        let src = r#"---
name: security-review
description: Claude implements, Codex reviews, GPT summarizes
rotation: [claude, codex, gpt]
prompts:
  codex: "Focus on injection and auth bypass."
  gpt: "Summarize in 3 bullets."
---

# Body

Some context.
"#;
        let skill =
            parse_skill_content(src, &p("/tmp/sec.md"), SkillScope::Project).expect("should parse");
        assert_eq!(skill.name, "security-review");
        assert_eq!(
            skill.description,
            "Claude implements, Codex reviews, GPT summarizes"
        );
        assert_eq!(
            skill.rotation,
            vec![Agent::Claude, Agent::Codex, Agent::Gpt]
        );
        assert_eq!(
            skill
                .per_agent_prompts
                .get(&Agent::Codex)
                .map(String::as_str),
            Some("Focus on injection and auth bypass.")
        );
        assert_eq!(
            skill.per_agent_prompts.get(&Agent::Gpt).map(String::as_str),
            Some("Summarize in 3 bullets.")
        );
        assert!(skill.body.starts_with("# Body"));
        assert_eq!(skill.scope, SkillScope::Project);
    }

    #[test]
    fn missing_frontmatter_is_error() {
        let src = "no frontmatter here\n";
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(err.message.contains("frontmatter"), "got: {}", err.message);
    }

    #[test]
    fn missing_required_fields() {
        let src = r#"---
description: x
rotation: [claude]
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(err.message.contains("name"), "got: {}", err.message);

        let src = r#"---
name: x
rotation: [claude]
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(err.message.contains("description"), "got: {}", err.message);

        let src = r#"---
name: x
description: y
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(err.message.contains("rotation"), "got: {}", err.message);
    }

    #[test]
    fn empty_rotation_is_error() {
        let src = r#"---
name: x
description: y
rotation: []
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(
            err.message.contains("at least one agent"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn unknown_agent_in_rotation() {
        let src = r#"---
name: x
description: y
rotation: [claude, gemini]
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(err.message.contains("gemini"), "got: {}", err.message);
    }

    #[test]
    fn invalid_name_rejected() {
        for (bad, why) in [
            ("Security-Review", "uppercase"),
            ("1security", "starts with digit"),
            ("sec--rev", "double hyphen ok actually under our rules"),
            ("sec-", "ends with hyphen"),
            ("sec_rev", "underscore"),
        ] {
            let src = format!(
                r#"---
name: {bad}
description: y
rotation: [claude]
---
"#
            );
            // sec--rev is actually allowed under our spec (it only forbids
            // start-with-digit, uppercase, trailing hyphen, non-[a-z0-9-]).
            // Skip that case.
            if bad == "sec--rev" {
                let _ = (bad, why);
                continue;
            }
            let err = parse_skill_content(&src, &p("/tmp/x.md"), SkillScope::Global)
                .expect_err(&format!("expected reject for {bad}: {why}"));
            assert!(err.message.contains("name"), "case {bad}: {}", err.message);
        }
    }

    #[test]
    fn malformed_yaml_is_error() {
        let src = r#"---
name security-review
description: y
rotation: [claude]
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        // missing `:` triggers "expected `key: value`"
        assert!(err.message.contains("key: value") || err.message.contains("name"));
    }

    #[test]
    fn unterminated_quoted_value() {
        let src = r#"---
name: "broken
description: y
rotation: [claude]
---
"#;
        let err = parse_skill_content(src, &p("/tmp/x.md"), SkillScope::Global).unwrap_err();
        assert!(
            err.message.contains("double-quoted"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn body_is_optional() {
        let src = r#"---
name: a
description: b
rotation: [claude]
---
"#;
        let skill = parse_skill_content(src, &p("/tmp/a.md"), SkillScope::Global).unwrap();
        assert!(skill.body.is_empty());
    }

    #[test]
    fn comments_in_frontmatter_are_stripped() {
        let src = r#"---
name: a       # the name
description: b  # the desc
rotation: [claude]
---
"#;
        let skill = parse_skill_content(src, &p("/tmp/a.md"), SkillScope::Global).unwrap();
        assert_eq!(skill.name, "a");
        assert_eq!(skill.description, "b");
    }

    #[test]
    fn loader_skips_non_md_and_collects_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::try_from(dir.path().to_path_buf()).unwrap();

        std::fs::write(
            path.join("good.md"),
            "---\nname: good\ndescription: ok\nrotation: [claude]\n---\n",
        )
        .unwrap();
        std::fs::write(path.join("README.txt"), "ignored").unwrap();
        std::fs::write(
            path.join("bad.md"),
            "---\nname: bad\ndescription: x\nrotation: [nope]\n---\n",
        )
        .unwrap();

        let (skills, errors) = load_skills_from_dir(&path, SkillScope::Global);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nope"));
    }

    #[test]
    fn loader_reports_intra_scope_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::try_from(dir.path().to_path_buf()).unwrap();

        // Both files declare `name: dup` — load order is alphabetical.
        std::fs::write(
            path.join("aaa.md"),
            "---\nname: dup\ndescription: first\nrotation: [claude]\n---\n",
        )
        .unwrap();
        std::fs::write(
            path.join("bbb.md"),
            "---\nname: dup\ndescription: second\nrotation: [gpt]\n---\n",
        )
        .unwrap();

        let (skills, errors) = load_skills_from_dir(&path, SkillScope::Project);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "first");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn nonexistent_dir_returns_empty() {
        let (skills, errors) = load_skills_from_dir(
            &p("/this/path/does/not/exist/relay-skills"),
            SkillScope::Global,
        );
        assert!(skills.is_empty());
        assert!(errors.is_empty());
    }
}

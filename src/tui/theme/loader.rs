//! JSON theme loading + var resolution.
//!
//! Modeled on pi-mono's `theme.ts`. A theme JSON has:
//! - optional `vars`: a map of named color variables
//! - required `colors`: 31 semantic tokens (see `theme-schema.json`)
//!
//! Each color value is either a hex string (`"#rrggbb"`), a 256-color index
//! integer (0..=255), an empty string (= terminal default), or a string that
//! references a key in `vars` (resolved transitively, with cycle detection).

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use ratatui::style::Color;
use serde::Deserialize;

use super::tokens::{ResolvedTheme, ALL_TOKEN_NAMES};

/// A color value as it appears in the JSON file (before variable resolution).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ColorValue {
    /// Hex `"#rrggbb"`, var reference, or empty string for "terminal default".
    Str(String),
    /// 256-color palette index 0..=255.
    Index(u8),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeJson {
    pub name: String,
    #[serde(default)]
    pub vars: HashMap<String, ColorValue>,
    pub colors: HashMap<String, ColorValue>,
    // Allow `$schema` to appear in JSON without rejecting unknown fields.
    #[serde(rename = "$schema", default)]
    #[allow(dead_code)]
    pub schema: Option<String>,
}

/// Parse a theme JSON string. Validates that all required tokens are present
/// and that variable references resolve.
pub fn parse_theme(label: &str, content: &str) -> Result<ResolvedTheme> {
    let theme: ThemeJson =
        serde_json::from_str(content).with_context(|| format!("parsing theme '{label}'"))?;

    // Check that all required tokens exist.
    let mut missing = Vec::new();
    for token in ALL_TOKEN_NAMES {
        if !theme.colors.contains_key(*token) {
            missing.push(*token);
        }
    }
    if !missing.is_empty() {
        bail!(
            "theme '{}' is missing required color tokens: {}",
            label,
            missing.join(", ")
        );
    }

    // Resolve every token through `vars`.
    let mut resolved: HashMap<String, Option<Color>> = HashMap::new();
    for token in ALL_TOKEN_NAMES {
        let value = theme
            .colors
            .get(*token)
            .ok_or_else(|| anyhow!("token '{}' missing after presence check", token))?;
        let color = resolve_value(value, &theme.vars, &mut HashSet::new())
            .with_context(|| format!("resolving token '{}' in theme '{}'", token, label))?;
        resolved.insert((*token).to_string(), color);
    }

    Ok(ResolvedTheme {
        name: theme.name,
        colors: resolved,
    })
}

fn resolve_value(
    value: &ColorValue,
    vars: &HashMap<String, ColorValue>,
    visited: &mut HashSet<String>,
) -> Result<Option<Color>> {
    match value {
        ColorValue::Index(idx) => Ok(Some(Color::Indexed(*idx))),
        ColorValue::Str(s) => {
            if s.is_empty() {
                // Empty string => terminal default (Color::Reset in ratatui).
                Ok(None)
            } else if let Some(stripped) = s.strip_prefix('#') {
                Ok(Some(parse_hex(stripped)?))
            } else {
                if visited.contains(s) {
                    bail!("circular variable reference: '{}'", s);
                }
                let next = vars
                    .get(s)
                    .ok_or_else(|| anyhow!("variable reference not found: '{}'", s))?;
                visited.insert(s.clone());
                let result = resolve_value(next, vars, visited);
                visited.remove(s);
                result
            }
        }
    }
}

fn parse_hex(hex: &str) -> Result<Color> {
    if hex.len() != 6 {
        bail!("invalid hex color (need 6 digits): '#{}'", hex);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).context("invalid hex red component")?;
    let g = u8::from_str_radix(&hex[2..4], 16).context("invalid hex green component")?;
    let b = u8::from_str_radix(&hex[4..6], 16).context("invalid hex blue component")?;
    Ok(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_theme_with(extra: &str) -> String {
        // Build a minimal JSON theme with all required tokens set to "#000000",
        // overriding any tokens listed in `extra` (already-formatted JSON pairs).
        let mut colors = String::new();
        for token in ALL_TOKEN_NAMES {
            colors.push_str(&format!("\"{}\": \"#000000\",", token));
        }
        // Trim trailing comma.
        if colors.ends_with(',') {
            colors.pop();
        }
        format!(
            "{{\"name\":\"t\",\"vars\":{{\"red\":\"#ff0000\"}},\"colors\":{{{}{}}}}}",
            colors,
            if extra.is_empty() {
                String::new()
            } else {
                format!(",{}", extra)
            }
        )
    }

    #[test]
    fn parses_minimal_theme() {
        let json = minimal_theme_with("");
        let theme = parse_theme("test", &json).expect("should parse");
        assert_eq!(theme.name, "t");
        assert_eq!(theme.colors.len(), ALL_TOKEN_NAMES.len());
    }

    #[test]
    fn resolves_var_reference() {
        let json = minimal_theme_with("\"accent\":\"red\"");
        let theme = parse_theme("test", &json).expect("should parse");
        assert_eq!(theme.colors["accent"], Some(Color::Rgb(0xff, 0, 0)));
    }

    #[test]
    fn empty_string_means_terminal_default() {
        let json = minimal_theme_with("\"text\":\"\"");
        let theme = parse_theme("test", &json).expect("should parse");
        assert_eq!(theme.colors["text"], None);
    }

    #[test]
    fn rejects_missing_token() {
        // A theme with no `bg` token.
        let mut colors = String::new();
        for token in ALL_TOKEN_NAMES {
            if *token == "bg" {
                continue;
            }
            colors.push_str(&format!("\"{}\": \"#000000\",", token));
        }
        if colors.ends_with(',') {
            colors.pop();
        }
        let json = format!("{{\"name\":\"t\",\"colors\":{{{}}}}}", colors);
        let err = parse_theme("test", &json).unwrap_err();
        assert!(err.to_string().contains("bg"));
    }

    fn err_chain(err: &anyhow::Error) -> String {
        err.chain()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn rejects_circular_var_reference() {
        let json = format!(
            "{{\"name\":\"t\",\"vars\":{{\"a\":\"b\",\"b\":\"a\"}},\"colors\":{{{}}}}}",
            ALL_TOKEN_NAMES
                .iter()
                .map(|t| format!("\"{}\":\"a\"", t))
                .collect::<Vec<_>>()
                .join(",")
        );
        let err = parse_theme("test", &json).unwrap_err();
        let chain = err_chain(&err);
        assert!(chain.contains("circular"), "got: {chain}");
    }

    #[test]
    fn rejects_unknown_var() {
        let json = minimal_theme_with("\"accent\":\"nope\"");
        let err = parse_theme("test", &json).unwrap_err();
        let chain = err_chain(&err);
        assert!(chain.contains("not found"), "got: {chain}");
    }

    #[test]
    fn accepts_256color_index() {
        let json = format!(
            "{{\"name\":\"t\",\"colors\":{{{}}}}}",
            ALL_TOKEN_NAMES
                .iter()
                .map(|t| format!("\"{}\":42", t))
                .collect::<Vec<_>>()
                .join(",")
        );
        let theme = parse_theme("test", &json).expect("should parse");
        assert_eq!(theme.colors["accent"], Some(Color::Indexed(42)));
    }
}

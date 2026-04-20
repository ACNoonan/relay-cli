//! JSON-loaded theme system for the relay TUIs.
//!
//! Modeled on pi-mono's [`theme.ts`]. Themes are JSON files (see
//! `assets/themes/theme-schema.json`) with two keyed sections: `vars`
//! (reusable color variables) and `colors` (~31 semantic tokens). Tokens may
//! reference vars by name; vars are resolved transitively with cycle
//! detection.
//!
//! # Selection
//!
//! At startup [`Styles::load`] picks a theme in this order:
//! 1. The `RELAY_THEME` environment variable.
//! 2. The `[ui].theme` value from `.agent-harness/config.toml`.
//! 3. The built-in default `"amber"` (relay's historical look).
//!
//! Built-in themes (`amber`, `dark`, `light`) are embedded via `include_str!`
//! and ship in the binary. User themes live in `~/.config/relay/themes/*.json`
//! (or wherever `XDG_CONFIG_HOME` points).
//!
//! [`theme.ts`]: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/modes/interactive/theme/theme.ts

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use ratatui::style::{Color, Modifier, Style};

use crate::tui::theme::tokens::ResolvedTheme;

mod loader;
mod tokens;

use loader::parse_theme;

const BUILTIN_AMBER: &str = include_str!("../../../assets/themes/amber.json");
const BUILTIN_DARK: &str = include_str!("../../../assets/themes/dark.json");
const BUILTIN_LIGHT: &str = include_str!("../../../assets/themes/light.json");

const DEFAULT_THEME_NAME: &str = "amber";

/// Resolved theme palette. Use [`Theme::token`] to get a `ratatui::Color` by
/// semantic name. Tokens with empty-string values resolve to
/// `Color::Reset` (= terminal default).
#[derive(Debug, Clone)]
pub struct Theme {
    resolved: ResolvedTheme,
}

impl Theme {
    /// Look up a token by name. Panics in debug builds if the token is
    /// unknown — this is a programmer error, not user-supplied input. In
    /// release builds returns `Color::Reset` to keep the TUI rendering.
    pub fn token(&self, name: &str) -> Color {
        match self.resolved.colors.get(name) {
            Some(Some(c)) => *c,
            Some(None) => Color::Reset,
            None => {
                debug_assert!(false, "unknown theme token: {name}");
                Color::Reset
            }
        }
    }

    /// Theme name (from the JSON `name` field). Public for use by future
    /// chat-side code (Wave B) and tests.
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.resolved.name
    }
}

/// Style helpers used by every screen. The method surface is intentionally
/// stable so screen files don't need updates when the underlying palette
/// changes.
pub struct Styles {
    pub theme: Theme,
}

impl Styles {
    /// Build a `Styles` instance from a resolved [`Theme`].
    pub fn from_theme(theme: Theme) -> Self {
        Self { theme }
    }

    /// Load the active theme using the selection order described in the
    /// module docs. On any failure (missing file, invalid JSON, missing
    /// tokens) falls back to the built-in default and emits a `tracing`
    /// warning so the TUI never refuses to start over a theme problem.
    pub fn load(harness_dir: Option<&camino::Utf8Path>) -> Self {
        let requested = pick_theme_name(harness_dir);
        match load_theme(&requested) {
            Ok(theme) => Self::from_theme(theme),
            Err(err) => {
                tracing::warn!(
                    requested = %requested,
                    error = %err,
                    "theme load failed; falling back to built-in '{}'",
                    DEFAULT_THEME_NAME
                );
                let theme = load_builtin(DEFAULT_THEME_NAME)
                    .expect("built-in default theme must always parse");
                Self::from_theme(theme)
            }
        }
    }

    /// Construct with the built-in default theme. Useful in tests and any
    /// caller that doesn't want disk I/O. Currently only exercised by tests
    /// and reserved for the upcoming chat-side adoption (Wave B).
    #[allow(dead_code)]
    pub fn builtin_default() -> Self {
        let theme =
            load_builtin(DEFAULT_THEME_NAME).expect("built-in default theme must always parse");
        Self::from_theme(theme)
    }

    fn fg_bg(&self, fg: &str, bg: &str) -> Style {
        Style::default()
            .fg(self.theme.token(fg))
            .bg(self.theme.token(bg))
    }

    pub fn base(&self) -> Style {
        self.fg_bg("text", "bg")
    }

    pub fn dim(&self) -> Style {
        self.fg_bg("dim", "bg")
    }

    pub fn accent(&self) -> Style {
        self.fg_bg("accent", "bg")
    }

    pub fn accent_bold(&self) -> Style {
        self.fg_bg("accentBright", "bg")
            .add_modifier(Modifier::BOLD)
    }

    pub fn header(&self) -> Style {
        self.fg_bg("headerFg", "headerBg")
            .add_modifier(Modifier::BOLD)
    }

    pub fn status_bar(&self) -> Style {
        self.fg_bg("text", "statusBarBg")
    }

    pub fn border(&self) -> Style {
        self.fg_bg("border", "bg")
    }

    pub fn border_active(&self) -> Style {
        self.fg_bg("borderAccent", "bg")
    }

    pub fn selected(&self) -> Style {
        self.fg_bg("selectedFg", "selectedBg")
            .add_modifier(Modifier::BOLD)
    }

    pub fn label(&self) -> Style {
        self.fg_bg("muted", "bg").add_modifier(Modifier::BOLD)
    }

    pub fn danger(&self) -> Style {
        self.fg_bg("error", "bg")
    }

    pub fn success(&self) -> Style {
        self.fg_bg("success", "bg")
    }

    pub fn warning(&self) -> Style {
        self.fg_bg("warning", "bg")
    }

    pub fn nav_item(&self, active: bool) -> Style {
        if active {
            Style::default()
                .fg(self.theme.token("accentBright"))
                .bg(self.theme.token("navActiveBg"))
                .add_modifier(Modifier::BOLD)
        } else {
            self.fg_bg("dim", "bg")
        }
    }

    pub fn status_style(&self, status: &str) -> Style {
        match status {
            "Running" => Style::default()
                .fg(self.theme.token("statusRunning"))
                .add_modifier(Modifier::BOLD),
            "Completed" | "Pass" => Style::default().fg(self.theme.token("statusPass")),
            "Crashed" | "Fail" => Style::default()
                .fg(self.theme.token("statusFail"))
                .add_modifier(Modifier::BOLD),
            "Stopped" | "NeedsWork" => Style::default().fg(self.theme.token("warning")),
            _ => Style::default().fg(self.theme.token("dim")),
        }
    }

    pub fn severity_style(&self, severity: &str) -> Style {
        match severity {
            "critical" => Style::default()
                .fg(self.theme.token("error"))
                .add_modifier(Modifier::BOLD),
            "high" => Style::default().fg(self.theme.token("error")),
            "medium" => Style::default().fg(self.theme.token("warning")),
            "low" => Style::default().fg(self.theme.token("text")),
            _ => Style::default().fg(self.theme.token("dim")),
        }
    }
}

// ---------------------------------------------------------------------------
// Selection + loading
// ---------------------------------------------------------------------------

fn pick_theme_name(harness_dir: Option<&camino::Utf8Path>) -> String {
    if let Ok(name) = std::env::var("RELAY_THEME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(dir) = harness_dir {
        let cfg = dir.join("config.toml");
        if let Ok(name) = read_theme_from_config(cfg.as_std_path()) {
            return name;
        }
    }
    DEFAULT_THEME_NAME.to_string()
}

fn read_theme_from_config(path: &std::path::Path) -> Result<String> {
    let bytes =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: toml::Value = toml::from_str(&bytes).context("parsing config.toml")?;
    let theme = value
        .get("ui")
        .and_then(|ui| ui.get("theme"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("[ui].theme not set"))?
        .trim();
    if theme.is_empty() {
        Err(anyhow!("[ui].theme is empty"))
    } else {
        Ok(theme.to_string())
    }
}

fn load_theme(name: &str) -> Result<Theme> {
    if let Ok(theme) = load_builtin(name) {
        return Ok(theme);
    }
    let path = custom_theme_path(name);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading custom theme '{}' at {}", name, path.display()))?;
    let resolved = parse_theme(name, &content)?;
    Ok(Theme { resolved })
}

fn load_builtin(name: &str) -> Result<Theme> {
    let content = match name {
        "amber" => BUILTIN_AMBER,
        "dark" => BUILTIN_DARK,
        "light" => BUILTIN_LIGHT,
        _ => return Err(anyhow!("not a built-in theme: '{}'", name)),
    };
    let resolved = parse_theme(name, content)?;
    Ok(Theme { resolved })
}

/// Resolve `~/.config/relay/themes/<name>.json` honoring `XDG_CONFIG_HOME`.
fn custom_theme_path(name: &str) -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("relay")
        .join("themes")
        .join(format!("{name}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_amber_loads() {
        let styles = Styles::builtin_default();
        assert_eq!(styles.theme.name(), "amber");
    }

    #[test]
    fn all_builtins_load() {
        for name in ["amber", "dark", "light"] {
            let theme = load_builtin(name)
                .unwrap_or_else(|e| panic!("built-in '{name}' should parse: {e}"));
            assert_eq!(theme.name(), name);
        }
    }

    #[test]
    fn env_var_overrides_default() {
        // SAFETY: tests run on a single thread within this module by default;
        // we restore the env var after the assertion.
        let original = std::env::var("RELAY_THEME").ok();
        std::env::set_var("RELAY_THEME", "dark");
        let name = pick_theme_name(None);
        match original {
            Some(v) => std::env::set_var("RELAY_THEME", v),
            None => std::env::remove_var("RELAY_THEME"),
        }
        assert_eq!(name, "dark");
    }

    #[test]
    fn config_file_provides_theme_when_no_env() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("config.toml");
        std::fs::write(&cfg, "[ui]\ntheme = \"light\"\n").expect("write config");

        let original = std::env::var("RELAY_THEME").ok();
        std::env::remove_var("RELAY_THEME");
        let utf8 =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 tmp path");
        let name = pick_theme_name(Some(utf8.as_path()));
        if let Some(v) = original {
            std::env::set_var("RELAY_THEME", v);
        }
        assert_eq!(name, "light");
    }

    #[test]
    fn unknown_theme_falls_back_to_default() {
        let original = std::env::var("RELAY_THEME").ok();
        std::env::set_var("RELAY_THEME", "no-such-theme-xyz");
        let styles = Styles::load(None);
        match original {
            Some(v) => std::env::set_var("RELAY_THEME", v),
            None => std::env::remove_var("RELAY_THEME"),
        }
        assert_eq!(styles.theme.name(), DEFAULT_THEME_NAME);
    }

    #[test]
    fn styles_render_without_panic() {
        let styles = Styles::builtin_default();
        // Just exercise every helper to make sure no token name typos slip in.
        let _ = styles.base();
        let _ = styles.dim();
        let _ = styles.accent();
        let _ = styles.accent_bold();
        let _ = styles.header();
        let _ = styles.status_bar();
        let _ = styles.border();
        let _ = styles.border_active();
        let _ = styles.selected();
        let _ = styles.label();
        let _ = styles.danger();
        let _ = styles.success();
        let _ = styles.warning();
        let _ = styles.nav_item(true);
        let _ = styles.nav_item(false);
        for s in [
            "Running",
            "Completed",
            "Pass",
            "Crashed",
            "Fail",
            "Stopped",
            "NeedsWork",
            "unknown",
        ] {
            let _ = styles.status_style(s);
        }
        for s in ["critical", "high", "medium", "low", "?"] {
            let _ = styles.severity_style(s);
        }
    }
}

//! Catalog of semantic theme tokens.
//!
//! Mirrors pi-mono's pattern but uses a relay-tailored subset (~31 tokens).
//! See `assets/themes/theme-schema.json` for full descriptions.

use std::collections::HashMap;

use ratatui::style::Color;

/// Every semantic token name relay supports. Used for validating that loaded
/// JSON has all required keys, and as the canonical lookup key set.
pub const ALL_TOKEN_NAMES: &[&str] = &[
    // Core UI
    "accent",
    "accentBright",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    // Text & background
    "text",
    "bg",
    "headerBg",
    "headerFg",
    "statusBarBg",
    // Selection / nav
    "selectedBg",
    "selectedFg",
    "navActiveBg",
    // Markdown (kept aligned with pi for Tier 1 #3)
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    // Status (relay-specific: maps cleanly to session status strings)
    "statusRunning",
    "statusPass",
    "statusFail",
];

/// A theme after JSON parsing + variable resolution. `None` means "use
/// terminal default" (corresponds to an empty-string color value, like pi's
/// `text: ""`).
#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    pub name: String,
    pub colors: HashMap<String, Option<Color>>,
}

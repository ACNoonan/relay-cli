mod actions;
mod app;
mod data;
mod events;
mod screens;
mod state;
mod theme;
mod widgets;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

/// Entry point for `relay tui`.
pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("getting cwd")?;
    let harness_dir = cwd.join(".agent-harness");
    let harness_root =
        Utf8PathBuf::from_path_buf(harness_dir).map_err(|_| anyhow::anyhow!("non-UTF8 path"))?;

    app::run_app(harness_root)
}

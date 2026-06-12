//! Development tasks for ripsync. Run with `cargo xtask <task>`.
//!
//! Tasks:
//!   - `dist-assets` (default): generate the man page and shell completions into
//!     `dist/assets/` for packaging.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let task = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dist-assets".into());
    match task.as_str() {
        "dist-assets" => dist_assets(),
        other => bail!("unknown task: {other}\nusage: cargo xtask dist-assets"),
    }
}

fn dist_assets() -> Result<()> {
    // Workspace root is the parent of this crate's manifest dir.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent dir")
        .to_path_buf();
    let assets = root.join("dist/assets");
    ripsync::write_assets(&assets).with_context(|| format!("writing assets to {assets:?}"))?;
    println!("generated man page + completions in {}", assets.display());
    Ok(())
}

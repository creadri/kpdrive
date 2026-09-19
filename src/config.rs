//! Settings the user can change, kept where XDG says settings go.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Days of activity log to keep. Older day-files are deleted.
    pub log_retention_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { log_retention_days: 30 }
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".config"))
        .join("kpdrive/config.json")
}

/// Never fails: a missing or unreadable config falls back to the defaults, so a
/// typo in the file cannot stop the daemon syncing.
pub fn load() -> Config {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) -> Result<()> {
    let path = path();
    std::fs::create_dir_all(path.parent().expect("config path has a parent"))?;
    std::fs::write(&path, serde_json::to_vec_pretty(config)?).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

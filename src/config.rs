//! Settings the user can change, kept where XDG says settings go.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How much of the activity log is worth keeping on disk. Ordered, so a line
/// is stored when its level is at least the configured one.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// The name as it appears in a log line and in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "INFO" => Some(Self::Info),
            "WARN" => Some(Self::Warn),
            "ERROR" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Days of activity log to keep. Older day-files are deleted.
    #[serde(default = "default_retention")]
    pub log_retention_days: u64,
    /// Where Drive is mirrored. Unset until the first `setup` or `--root`.
    #[serde(default)]
    pub sync_folder: Option<PathBuf>,
    /// Whether the daemon also brings down the Proton Photos timeline. Off by
    /// default: it is a second library, and a large one on most accounts.
    #[serde(default)]
    pub photos_sync: bool,
    /// Where the timeline is copied to. Unset means `<Pictures>/ProtonDrive`.
    #[serde(default)]
    pub photos_sync_folder: Option<PathBuf>,
    /// A folder whose photos are uploaded into Proton Photos and then removed
    /// from it. Unset means no ingestion.
    #[serde(default)]
    pub photos_ingestion_folder: Option<PathBuf>,
    /// After a photo is uploaded from the ingestion folder, delete it outright
    /// rather than moving it to the desktop trash.
    #[serde(default)]
    pub photos_ingestion_perm_rm: bool,
    /// The least severe line worth storing. Ordinary activity is still printed
    /// by the CLI, it just does not reach the log file below this.
    #[serde(default = "default_level")]
    pub log_level: LogLevel,
}

fn default_level() -> LogLevel {
    LogLevel::Warn
}

fn default_retention() -> u64 {
    30
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_retention_days: default_retention(),
            sync_folder: None,
            photos_sync: false,
            photos_sync_folder: None,
            photos_ingestion_folder: None,
            photos_ingestion_perm_rm: false,
            log_level: default_level(),
        }
    }
}

/// `~/ProtonDrive`, used until the user picks somewhere else.
pub fn default_sync_folder() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join("ProtonDrive"))
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

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

/// Where the machine draws power from, as Plasma's power management tells the
/// three apart.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    Ac,
    Battery,
    LowBattery,
}

impl Power {
    pub const ALL: [Self; 3] = [Self::Ac, Self::Battery, Self::LowBattery];

    /// The name as it appears in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ac => "ac",
            Self::Battery => "battery",
            Self::LowBattery => "low_battery",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// What each account has of its own: where it syncs, and what it does with
/// Proton Photos.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AccountConfig {
    /// The Proton username, as the session gave it.
    pub username: String,
    /// Where Drive is mirrored. Set when the account is added.
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
}

/// The per-account settings as a single-account version kept them, at the top
/// level. Read so the first account can take them over, never written back.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Legacy {
    #[serde(default)]
    pub sync_folder: Option<PathBuf>,
    #[serde(default)]
    pub photos_sync: bool,
    #[serde(default)]
    pub photos_sync_folder: Option<PathBuf>,
    #[serde(default)]
    pub photos_ingestion_folder: Option<PathBuf>,
    #[serde(default)]
    pub photos_ingestion_perm_rm: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Days of activity log to keep. Older day-files are deleted.
    #[serde(default = "default_retention")]
    pub log_retention_days: u64,
    /// The least severe line worth storing. Ordinary activity is still printed
    /// by the CLI, it just does not reach the log file below this.
    #[serde(default = "default_level")]
    pub log_level: LogLevel,
    /// Paused by hand: no file sync, photo download or ingestion until resumed.
    #[serde(default)]
    pub sync_paused: bool,
    /// NetworkManager connection names that pause syncing while connected,
    /// such as a phone's hotspot.
    #[serde(default)]
    pub pause_on_networks: Vec<String>,
    /// Power sources that pause syncing while in use. A low battery by default,
    /// so a sync does not drain what is left.
    #[serde(default = "default_power_pause")]
    pub pause_on_power: Vec<Power>,
    /// Every account, in the order they were added. The first is the default.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    #[serde(flatten, skip_serializing)]
    pub legacy: Legacy,
}

impl Config {
    /// The account called `username`, which Proton compares without case.
    pub fn account(&self, username: &str) -> Option<&AccountConfig> {
        self.accounts.iter().find(|a| a.username.eq_ignore_ascii_case(username))
    }

    pub fn account_mut(&mut self, username: &str) -> Option<&mut AccountConfig> {
        self.accounts.iter_mut().find(|a| a.username.eq_ignore_ascii_case(username))
    }
}

fn default_power_pause() -> Vec<Power> {
    vec![Power::LowBattery]
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
            log_level: default_level(),
            sync_paused: false,
            pause_on_networks: Vec::new(),
            pause_on_power: default_power_pause(),
            accounts: Vec::new(),
            legacy: Legacy::default(),
        }
    }
}

/// `~/.local/share/kpdrive`, where state and logs live.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share"))
        .join("kpdrive")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn old_settings_are_read_but_not_written() {
        let old = r#"{"sync_folder":"/home/me/ProtonDrive","photos_sync":true,"log_level":"INFO"}"#;
        let config: Config = serde_json::from_str(old).unwrap();
        assert_eq!(config.legacy.sync_folder.as_deref(), Some(Path::new("/home/me/ProtonDrive")));
        assert!(config.legacy.photos_sync);
        assert_eq!(config.log_level, LogLevel::Info);
        assert!(config.accounts.is_empty());
        let written = serde_json::to_string(&config).unwrap();
        assert!(!written.contains("sync_folder"), "{written}");
        assert!(!written.contains("photos_sync"), "{written}");
    }

    #[test]
    fn accounts_are_found_without_case() {
        let mut config = Config::default();
        config.accounts.push(AccountConfig { username: "Alice".into(), ..Default::default() });
        assert!(config.account("alice").is_some());
        assert!(config.account("bob").is_none());
        config.account_mut("ALICE").unwrap().photos_sync = true;
        let back: Config = serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(back.accounts[0].photos_sync);
    }
}

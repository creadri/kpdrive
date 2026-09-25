//! The accounts kpdrive knows, their stored sessions, and the account
//! operations the CLI, the daemon and the UI share.
//!
//! Each account's session lives in the system keyring (KWallet via the Secret
//! Service interface), never on disk in the clear. Its settings are an entry in
//! `config.json`, and its sync state a directory of its own; see
//! docs/multi-account-plan.md.

use anyhow::{Context, Result, anyhow, bail};
use proton_crypto::crypto::PGPProviderSync;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

use crate::api::{Api, Session};
use crate::config::{self, AccountConfig};
use crate::drive::Drive;

/// One Proton account, by the username it signed in with. Cheap: everything
/// else is read from the config or the keyring when asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub username: String,
}

/// Every known account, in the order they were added.
pub fn all() -> Vec<Account> {
    config::load().accounts.into_iter().map(|a| Account { username: a.username }).collect()
}

/// The account a command is for. `name` matches a username without regard to
/// case, or failing that the start of exactly one. Without a name there has
/// to be exactly one account to take.
pub fn select(name: Option<&str>) -> Result<Account> {
    pick(&all(), name)
}

fn pick(accounts: &[Account], name: Option<&str>) -> Result<Account> {
    let names = || accounts.iter().map(|a| a.username.as_str()).collect::<Vec<_>>().join(", ");
    let Some(name) = name else {
        return match accounts {
            [] => Err(anyhow!("not logged in, run `kpdrive login`")),
            [only] => Ok(only.clone()),
            _ => Err(anyhow!("several accounts are signed in ({}); pick one with --account NAME", names())),
        };
    };
    if let Some(exact) = accounts.iter().find(|a| a.username.eq_ignore_ascii_case(name)) {
        return Ok(exact.clone());
    }
    let lower = name.to_lowercase();
    let starts: Vec<&Account> = accounts.iter().filter(|a| a.username.to_lowercase().starts_with(&lower)).collect();
    match starts.as_slice() {
        [one] => Ok((*one).clone()),
        [] if accounts.is_empty() => Err(anyhow!("not logged in, run `kpdrive login`")),
        [] => Err(anyhow!("no account called {name}; known: {}", names())),
        _ => Err(anyhow!("{name} could be any of {}; say which", starts.iter().map(|a| a.username.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// The account whose sync folder holds `path`, if any does.
pub fn for_path(path: &Path) -> Option<Account> {
    all().into_iter().filter(|a| a.sync_folder().is_some_and(|root| path.starts_with(root))).max_by_key(|a| a.sync_folder().map(|r| r.as_os_str().len()))
}

/// The username as it is safe to use in a file name: lower case, and nothing
/// but letters, digits and `._@-`.
pub fn slug(username: &str) -> String {
    let mut out: String = username
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._@-".contains(c) { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with('.') {
        out.insert(0, '_');
    }
    out
}

impl Account {
    pub fn slug(&self) -> String {
        slug(&self.username)
    }

    /// Where this account's sync, photos and ingestion state is kept.
    pub fn dir(&self) -> PathBuf {
        config::data_dir().join("accounts").join(self.slug())
    }

    /// This account's settings. An account removed meanwhile reads as one
    /// with nothing set.
    pub fn settings(&self) -> AccountConfig {
        config::load()
            .account(&self.username)
            .cloned()
            .unwrap_or_else(|| AccountConfig { username: self.username.clone(), ..Default::default() })
    }

    /// Loads the config, changes this account's entry, and saves it.
    pub fn update(&self, change: impl FnOnce(&mut AccountConfig)) -> Result<()> {
        let mut config = config::load();
        let entry = config.account_mut(&self.username).with_context(|| format!("{} is no longer an account here", self.username))?;
        change(entry);
        config::save(&config)
    }

    pub fn sync_folder(&self) -> Option<PathBuf> {
        self.settings().sync_folder.filter(|d| !d.as_os_str().is_empty())
    }

    /// Where the photos timeline is copied to.
    pub fn photos_folder(&self) -> Result<PathBuf> {
        match self.settings().photos_sync_folder.filter(|d| !d.as_os_str().is_empty()) {
            Some(dir) => Ok(dir),
            None => crate::photos::default_dest(),
        }
    }

    /// The folder photos are uploaded from, when one is set.
    pub fn ingest_folder(&self) -> Option<PathBuf> {
        self.settings().photos_ingestion_folder.filter(|d| !d.as_os_str().is_empty())
    }

    /// Every folder this account uses.
    pub fn folders(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.sync_folder().into_iter().collect();
        out.extend(self.photos_folder().ok());
        out.extend(self.ingest_folder());
        out
    }

    /// Every folder the other accounts use, which this one must stay out of.
    pub fn others_folders(&self) -> Vec<PathBuf> {
        all().into_iter().filter(|a| a != self).flat_map(|a| a.folders()).collect()
    }

    /// Refuses `folder` if it overlaps a folder of another account.
    pub fn check_folder(&self, folder: &Path) -> Result<()> {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        let mine = canon(folder);
        for other in all().into_iter().filter(|a| a != self) {
            for theirs in other.folders() {
                if overlaps(&mine, &canon(&theirs)) {
                    bail!("{} overlaps {}, which belongs to {}; pick a folder of its own", folder.display(), theirs.display(), other.username);
                }
            }
        }
        Ok(())
    }

    fn attrs(&self) -> HashMap<&'static str, &str> {
        HashMap::from([("application", "kpdrive"), ("account", self.username.as_str())])
    }

    pub async fn session(&self) -> Result<Option<Session>> {
        let items = keyring().await?.search_items(&self.attrs()).await?;
        let Some(item) = items.first() else { return Ok(None) };
        let secret = item.secret().await?;
        Ok(Some(serde_json::from_slice(&secret).context("stored session is corrupt")?))
    }

    pub async fn save_session(&self, session: &Session) -> Result<()> {
        let json = Zeroizing::new(serde_json::to_vec(session)?);
        keyring()
            .await?
            .create_item(&format!("Proton Drive session for {} (kpdrive)", self.username), &self.attrs(), json.as_slice(), true)
            .await?;
        Ok(())
    }

    pub async fn open_drive(&self) -> Result<(Drive<impl PGPProviderSync>, Session)> {
        let session = self
            .session()
            .await?
            .with_context(|| format!("{} is not logged in, run `kpdrive login`", self.username))?;
        let drive = Drive::open(Api::new(Some(session.clone())), proton_crypto::new_pgp_provider()).await?;
        Ok((drive, session))
    }

    /// Stores rotated tokens if a refresh happened during a command.
    pub async fn persist(&self, api: Api, before: Session) -> Result<()> {
        if let Some(s) = api.session().filter(|s| *s != before) {
            self.save_session(&s).await?;
        }
        Ok(())
    }

    pub async fn info(&self) -> Result<Info> {
        let session = self.session().await?.with_context(|| format!("{} is not logged in", self.username))?;
        let api = Api::new(Some(session.clone()));
        let user = api.user().await?;
        api.key_secret()?; // the stored session must still carry the key secret
        let info = Info { username: user.name, used_bytes: user.used_space, total_bytes: user.max_space };
        self.persist(api, session).await?;
        Ok(info)
    }

    /// Ends the session server-side, then forgets it locally. The local half
    /// runs even if the server call fails, or a revoked session would be
    /// stuck here. The account and its folder stay.
    pub async fn logout(&self) -> Result<()> {
        if let Some(session) = self.session().await? {
            let api = Api::new(Some(session));
            if let Err(e) = api.delete::<serde_json::Value>("auth/v4").await {
                crate::log::warn(&format!("server-side logout failed ({e}); forgetting the session anyway"));
            }
        }
        keyring().await?.delete(&self.attrs()).await?;
        crate::log::write("INFO", &format!("logged out of {}", self.username));
        Ok(())
    }

    /// Signs out and forgets the account: its settings and sync state. The
    /// files in its folders are left where they are.
    pub async fn remove(&self) -> Result<()> {
        self.logout().await?;
        let mut config = config::load();
        config.accounts.retain(|a| !a.username.eq_ignore_ascii_case(&self.username));
        config::save(&config)?;
        match std::fs::remove_dir_all(self.dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(anyhow::Error::from(e).context(format!("remove {}", self.dir().display()))),
            _ => {}
        }
        crate::log::write("INFO", &format!("removed the account {}", self.username));
        Ok(())
    }
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("open Secret Service keyring (is KWallet running?)")
}

/// What the account screen shows.
pub struct Info {
    pub username: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// Signs in through the browser. `show` receives the URL to open and the code
/// the user must confirm there. Adds the account, or refreshes the session of
/// one already known; `true` when it is new.
pub async fn login(show: impl FnOnce(&str, &str)) -> Result<(Account, bool)> {
    let api = Api::new(None);
    api.login_via_browser(show).await?;
    let session = api.take_session().expect("login sets session");
    let mut config = config::load();
    if let Some(known) = config.account(&session.username) {
        let account = Account { username: known.username.clone() };
        account.save_session(&session).await?;
        crate::log::write("INFO", &format!("logged in as {}", account.username));
        return Ok((account, false));
    }
    // The first account takes over whatever a single-account version left.
    let settings = match config.accounts.is_empty() {
        true => adopt_legacy(&config, &session.username)?,
        false => fresh(&config, &session.username)?,
    };
    let account = Account { username: settings.username.clone() };
    account.save_session(&session).await?;
    config.accounts.push(settings);
    config::save(&config)?;
    crate::log::write("INFO", &format!("added the account {}", account.username));
    Ok((account, true))
}

/// Settings for an account added next to others: folders nobody uses yet.
fn fresh(config: &config::Config, username: &str) -> Result<AccountConfig> {
    let taken: Vec<PathBuf> = config
        .accounts
        .iter()
        .flat_map(|a| {
            let account = Account { username: a.username.clone() };
            account.folders()
        })
        .collect();
    let slug = slug(username);
    Ok(AccountConfig {
        username: username.to_owned(),
        sync_folder: Some(free_folder(config::default_sync_folder()?, &slug, &taken)),
        photos_sync_folder: Some(free_folder(crate::photos::default_dest()?, &slug, &taken)),
        ..Default::default()
    })
}

/// `base` if no account uses it and it holds nothing, otherwise `base-<slug>`.
fn free_folder(base: PathBuf, slug: &str, taken: &[PathBuf]) -> PathBuf {
    let empty = std::fs::read_dir(&base).map(|mut d| d.next().is_none()).unwrap_or(true);
    if empty && !taken.iter().any(|t| overlaps(t, &base)) {
        return base;
    }
    let name = base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    base.with_file_name(format!("{name}-{slug}"))
}

/// Moves a single-account version's state into `username`'s directory, and
/// builds its settings from the old top-level ones. Safe to run again after
/// it was interrupted: a file already moved is not moved twice.
fn adopt_legacy(config: &config::Config, username: &str) -> Result<AccountConfig> {
    let account = Account { username: username.to_owned() };
    adopt_files(config, username, &config::data_dir(), &account.dir())
}

/// [`adopt_legacy`], with the directories spelled out: `old` is where a
/// single-account version kept its state, `dir` the account's own.
fn adopt_files(config: &config::Config, username: &str, old: &Path, dir: &Path) -> Result<AccountConfig> {
    // Folders an older version only recorded in its state files.
    let recorded = |file: &str, key: &str| -> Option<PathBuf> {
        let bytes = std::fs::read(old.join(file)).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        v[key].as_str().filter(|s| !s.is_empty()).map(PathBuf::from)
    };
    let legacy = &config.legacy;
    let settings = AccountConfig {
        username: username.to_owned(),
        sync_folder: Some(match legacy.sync_folder.clone().or_else(|| recorded("state.json", "root")) {
            Some(dir) => dir,
            None => config::default_sync_folder()?,
        }),
        photos_sync: legacy.photos_sync,
        photos_sync_folder: legacy.photos_sync_folder.clone().or_else(|| recorded("photos.json", "dest")),
        photos_ingestion_folder: legacy.photos_ingestion_folder.clone(),
        photos_ingestion_perm_rm: legacy.photos_ingestion_perm_rm,
    };
    std::fs::create_dir_all(dir)?;
    for file in ["state.json", "photos.json", "ingest.json"] {
        let (from, to) = (old.join(file), dir.join(file));
        if from.exists() && !to.exists() {
            std::fs::rename(&from, &to).with_context(|| format!("move {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(settings)
}

/// Brings a single-account setup over to the per-account layout, once. Runs
/// at the start of every kpdrive program; a no-op once there are accounts, or
/// when there is no old session to move. Each step can be repeated, so an
/// interrupted run finishes next time.
pub async fn migrate() -> Result<()> {
    let mut config = config::load();
    if !config.accounts.is_empty() {
        return Ok(());
    }
    let keyring = keyring().await?;
    for item in keyring.search_items(&HashMap::from([("application", "kpdrive")])).await? {
        if item.attributes().await?.contains_key("account") {
            continue;
        }
        let session: Session = serde_json::from_slice(&item.secret().await?).context("stored session is corrupt")?;
        let settings = adopt_legacy(&config, &session.username)?;
        let account = Account { username: settings.username.clone() };
        account.save_session(&session).await?;
        config.accounts.push(settings);
        config::save(&config)?;
        item.delete().await?;
        crate::log::write("INFO", &format!("moved {} to the per-account layout", account.username));
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<Account> {
        names.iter().map(|n| Account { username: (*n).into() }).collect()
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(slug("Alice"), "alice");
        assert_eq!(slug("alice@proton.me"), "alice@proton.me");
        assert_eq!(slug("a/b c"), "a_b_c");
        assert_eq!(slug(".."), "_..");
        assert_eq!(slug(""), "_");
    }

    #[test]
    fn picking_an_account() {
        assert!(pick(&[], None).unwrap_err().to_string().contains("not logged in"));
        assert!(pick(&[], Some("alice")).unwrap_err().to_string().contains("not logged in"));
        let one = named(&["alice"]);
        assert_eq!(pick(&one, None).unwrap().username, "alice");
        let two = named(&["alice", "alicia", "bob"]);
        assert!(pick(&two, None).unwrap_err().to_string().contains("--account"));
        assert_eq!(pick(&two, Some("BOB")).unwrap().username, "bob");
        assert_eq!(pick(&two, Some("b")).unwrap().username, "bob");
        assert_eq!(pick(&two, Some("alice")).unwrap().username, "alice", "an exact name beats a prefix");
        assert!(pick(&two, Some("ali")).unwrap_err().to_string().contains("any of"));
        assert!(pick(&two, Some("carol")).unwrap_err().to_string().contains("no account called"));
    }

    #[test]
    fn a_single_account_setup_is_adopted() {
        let old = std::env::temp_dir().join(format!("kpdrive-adopt-{}", std::process::id()));
        let dir = old.join("accounts/alice");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("state.json"), r#"{"root":"/home/me/Drive","event_id":null,"nodes":{}}"#).unwrap();
        std::fs::write(old.join("photos.json"), r#"{"dest":"/home/me/Pics","photos":{}}"#).unwrap();
        let mut config = config::Config::default();
        config.legacy.photos_sync = true;
        config.legacy.photos_ingestion_folder = Some("/home/me/Phone".into());

        let adopted = adopt_files(&config, "alice", &old, &dir).unwrap();
        assert_eq!(adopted.username, "alice");
        assert_eq!(adopted.sync_folder.as_deref(), Some(Path::new("/home/me/Drive")), "from the old state");
        assert_eq!(adopted.photos_sync_folder.as_deref(), Some(Path::new("/home/me/Pics")), "from the old photo state");
        assert!(adopted.photos_sync);
        assert_eq!(adopted.photos_ingestion_folder.as_deref(), Some(Path::new("/home/me/Phone")));
        assert!(dir.join("state.json").exists() && !old.join("state.json").exists());
        assert!(dir.join("photos.json").exists());

        // Again, as after an interruption: nothing moved twice, nothing lost.
        std::fs::write(old.join("state.json"), b"stale").unwrap();
        config.legacy.sync_folder = Some("/home/me/Configured".into());
        let again = adopt_files(&config, "alice", &old, &dir).unwrap();
        assert_eq!(again.sync_folder.as_deref(), Some(Path::new("/home/me/Configured")), "the setting beats the old state");
        assert!(std::fs::read_to_string(dir.join("state.json")).unwrap().contains("/home/me/Drive"), "the moved state is kept");
        std::fs::remove_dir_all(&old).unwrap();
    }

    #[test]
    fn new_accounts_get_folders_of_their_own() {
        let dir = std::env::temp_dir().join(format!("kpdrive-free-{}", std::process::id()));
        let base = dir.join("ProtonDrive");
        assert_eq!(free_folder(base.clone(), "bob", &[]), base, "missing and unused");
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(free_folder(base.clone(), "bob", &[]), base, "empty and unused");
        assert_eq!(free_folder(base.clone(), "bob", &[base.clone()]), dir.join("ProtonDrive-bob"), "another account's");
        std::fs::write(base.join("left.txt"), b"x").unwrap();
        assert_eq!(free_folder(base.clone(), "bob", &[]), dir.join("ProtonDrive-bob"), "holds files");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

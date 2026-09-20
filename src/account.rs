//! The stored session and the account operations the CLI and the UI share.
//!
//! The session lives in the system keyring (KWallet via the Secret Service
//! interface), never on disk in the clear.

use anyhow::{Context, Result};
use proton_crypto::crypto::PGPProviderSync;
use std::collections::HashMap;
use zeroize::Zeroizing;

use crate::api::{Api, Session};
use crate::drive::Drive;

fn attrs() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "kpdrive")])
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("open Secret Service keyring (is KWallet running?)")
}

pub async fn load() -> Result<Option<Session>> {
    let items = keyring().await?.search_items(&attrs()).await?;
    let Some(item) = items.first() else { return Ok(None) };
    let secret = item.secret().await?;
    Ok(Some(serde_json::from_slice(&secret).context("stored session is corrupt")?))
}

pub async fn save(session: &Session) -> Result<()> {
    let json = Zeroizing::new(serde_json::to_vec(session)?);
    keyring()
        .await?
        .create_item("Proton Drive session (kpdrive)", &attrs(), json.as_slice(), true)
        .await?;
    Ok(())
}

/// Signs in through the browser. `show` receives the URL to open and the code
/// the user must confirm there. Returns the username.
pub async fn login(show: impl FnOnce(&str, &str)) -> Result<String> {
    let api = Api::new(None);
    api.login_via_browser(show).await?;
    let session = api.take_session().expect("login sets session");
    save(&session).await?;
    crate::log::write("INFO", &format!("logged in as {}", session.username));
    Ok(session.username)
}

/// Ends the session server-side, then forgets it locally. The local half runs
/// even if the server call fails, or a revoked session would be stuck here.
pub async fn logout() -> Result<()> {
    if let Some(session) = load().await? {
        let api = Api::new(Some(session));
        if let Err(e) = api.delete::<serde_json::Value>("auth/v4").await {
            crate::log::warn(&format!("server-side logout failed ({e}); forgetting the session anyway"));
        }
    }
    keyring().await?.delete(&attrs()).await?;
    crate::log::write("INFO", "logged out");
    Ok(())
}

/// What the account screen shows.
pub struct Info {
    pub username: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

pub async fn info() -> Result<Info> {
    let session = load().await?.context("not logged in")?;
    let api = Api::new(Some(session.clone()));
    let user = api.user().await?;
    api.key_secret()?; // the stored session must still carry the key secret
    let info = Info { username: user.name, used_bytes: user.used_space, total_bytes: user.max_space };
    persist(api, session).await?;
    Ok(info)
}

pub async fn open_drive() -> Result<(Drive<impl PGPProviderSync>, Session)> {
    let session = load().await?.context("not logged in, run `kpdrive login`")?;
    let drive = Drive::open(Api::new(Some(session.clone())), proton_crypto::new_pgp_provider()).await?;
    Ok((drive, session))
}

/// Stores rotated tokens if a refresh happened during a command.
pub async fn persist(api: Api, before: Session) -> Result<()> {
    if let Some(s) = api.session().filter(|s| *s != before) {
        save(&s).await?;
    }
    Ok(())
}

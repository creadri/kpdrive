mod api;
mod drive;

use anyhow::{Context, Result};
use api::{Api, Session};
use drive::Drive;
use proton_crypto::crypto::PGPProviderSync;
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(version, about = "Proton Drive sync client for KDE Plasma")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in via the browser and store the session in KWallet.
    Login,
    /// Show the account behind the stored session.
    Status,
    /// End the session server-side and forget it.
    Logout,
    /// List a remote folder.
    Ls {
        #[arg(default_value = "/")]
        path: String,
    },
    /// Download a remote file.
    Get {
        remote: String,
        /// Local destination; defaults to the file's name in the current directory.
        local: Option<std::path::PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Login => login().await,
        Cmd::Status => status().await,
        Cmd::Logout => logout().await,
        Cmd::Ls { path } => ls(&path).await,
        Cmd::Get { remote, local } => get(&remote, local).await,
    }
}

async fn login() -> Result<()> {
    let mut api = Api::new(None);
    api.login_via_browser(|url, code| {
        println!("Sign in in your browser. Confirm this code there: {code}\n{url}");
        if std::process::Command::new("xdg-open").arg(url).spawn().is_err() {
            eprintln!("could not run xdg-open; open the URL above manually");
        }
        // Fire-and-forget: kdialog would block on OK, and the browser is what matters.
        let _ = std::process::Command::new("kdialog")
            .args(["--title", "kpdrive", "--passivepopup", &format!("Confirm code {code} in your browser"), "30"])
            .spawn();
    })
    .await?;
    let session = api.session.take().expect("login sets session");
    save_session(&session).await?;
    println!("Logged in as {}. Session stored in the wallet.", session.username);
    Ok(())
}

async fn status() -> Result<()> {
    let session = load_session().await?.context("not logged in, run `kpdrive login`")?;
    let mut api = Api::new(Some(session.clone()));
    let user = api.user().await?;
    api.key_secret()?; // present in the stored session
    println!(
        "{} ({}): {:.1} / {:.1} GiB used",
        user.name,
        user.id,
        user.used_space as f64 / 1_073_741_824.0,
        user.max_space as f64 / 1_073_741_824.0
    );
    persist(api, session).await
}

async fn ls(path: &str) -> Result<()> {
    let (mut drive, before) = open_drive().await?;
    let folder = drive.resolve(path).await?;
    for n in drive.list(&folder).await? {
        println!("{} {}", if n.is_folder { "d" } else { "-" }, n.name);
    }
    persist(drive.api, before).await
}

async fn get(remote: &str, local: Option<std::path::PathBuf>) -> Result<()> {
    let (mut drive, before) = open_drive().await?;
    let file = drive.resolve(remote).await?;
    let dest = local.unwrap_or_else(|| std::path::PathBuf::from(&file.name));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?);
    let n = drive.download(&file, &mut out).await?;
    std::io::Write::flush(&mut out)?;
    println!("{} bytes -> {}", n, dest.display());
    persist(drive.api, before).await
}

async fn open_drive() -> Result<(Drive<impl PGPProviderSync>, Session)> {
    let session = load_session().await?.context("not logged in, run `kpdrive login`")?;
    let drive = Drive::open(Api::new(Some(session.clone())), proton_crypto::new_pgp_provider()).await?;
    Ok((drive, session))
}

/// Persist rotated tokens if a refresh happened during the command.
async fn persist(api: Api, before: Session) -> Result<()> {
    if let Some(s) = api.session.filter(|s| *s != before) {
        save_session(&s).await?;
    }
    Ok(())
}

async fn logout() -> Result<()> {
    if let Some(session) = load_session().await? {
        let mut api = Api::new(Some(session));
        if let Err(e) = api.delete::<serde_json::Value>("auth/v4").await {
            eprintln!("server-side logout failed ({e}); forgetting session anyway");
        }
    }
    keyring().await?.delete(&attrs()).await?;
    println!("Logged out.");
    Ok(())
}

fn attrs() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "kpdrive")])
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("open Secret Service keyring (is KWallet running?)")
}

async fn load_session() -> Result<Option<Session>> {
    let items = keyring().await?.search_items(&attrs()).await?;
    let Some(item) = items.first() else { return Ok(None) };
    let secret = item.secret().await?;
    Ok(Some(serde_json::from_slice(&secret).context("stored session is corrupt")?))
}

async fn save_session(s: &Session) -> Result<()> {
    let json = Zeroizing::new(serde_json::to_vec(s)?);
    keyring()
        .await?
        .create_item("Proton Drive session (kpdrive)", &attrs(), json.as_slice(), true)
        .await?;
    Ok(())
}

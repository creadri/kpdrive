
use kpdrive::{account, config, daemon, log, photos, setup, sync};
use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};

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
    /// Show or search the activity log.
    Logs {
        /// Only lines containing this text (case-insensitive).
        #[arg(long, default_value = "")]
        search: String,
        /// How many lines to show.
        #[arg(long, default_value_t = 200)]
        lines: usize,
        /// Set how many days of log to keep, and save it.
        #[arg(long)]
        retention: Option<u64>,
    },
    /// End the session server-side and forget it.
    Logout,
    /// List a remote folder.
    Ls {
        #[arg(default_value = "/")]
        path: String,
    },
    /// Create the local folder, add a Dolphin Places entry, and autostart the daemon.
    Setup {
        /// Local folder. Default: ~/ProtonDrive
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Download the Proton Photos timeline (one-way, remote → local).
    Photos {
        /// Where to put them. Default: ~/Pictures/Proton Drive
        #[arg(long)]
        dest: Option<std::path::PathBuf>,
    },
    /// Sync Drive with the local folder (two-way). --watch keeps running with a tray icon.
    Sync {
        /// Local folder; remembered after the first run. Default: ~/ProtonDrive
        #[arg(long)]
        root: Option<std::path::PathBuf>,
        /// Keep running and re-sync whenever the volume changes.
        #[arg(long)]
        watch: bool,
        /// Walk the tree even if no change was reported.
        #[arg(long)]
        force: bool,
    },
    /// Upload a local file into a remote folder (new file or new revision).
    Put {
        local: std::path::PathBuf,
        #[arg(default_value = "/")]
        remote_folder: String,
    },
    /// Create (or show) a public link for a file or folder. Accepts a remote
    /// path or a local one inside the sync folder.
    Share {
        remote: String,
        /// Put the link on the clipboard and show a notification.
        #[arg(long)]
        copy: bool,
        /// Extra password a recipient must type. It is NOT part of the link, so
        /// send it separately.
        #[arg(long)]
        password: Option<String>,
        /// Expire the link after this many days.
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// Remove the public link(s) from a remote file or folder.
    Unshare {
        remote: String,
    },
    /// Create a remote folder.
    Mkdir {
        remote: String,
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
        Cmd::Logs { search, lines, retention } => logs(&search, lines, retention),
        Cmd::Logout => do_logout().await,
        Cmd::Ls { path } => ls(&path).await,
        Cmd::Get { remote, local } => get(&remote, local).await,
        Cmd::Sync { root, watch, force } => sync(root, watch, force).await,
        Cmd::Photos { dest } => photos(dest).await,
        Cmd::Put { local, remote_folder } => put(&local, &remote_folder).await,
        Cmd::Setup { root } => setup(root).await,
        Cmd::Mkdir { remote } => mkdir(&remote).await,
        Cmd::Share { remote, copy, password, expires_days } => share(&remote, copy, password.as_deref(), expires_days).await,
        Cmd::Unshare { remote } => unshare(&remote).await,
    }
}

async fn login() -> Result<()> {
    let username = account::login(|url, code| {
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
    println!("logged in as {username}");
    Ok(())
}

fn logs(search: &str, lines: usize, retention: Option<u64>) -> Result<()> {
    let mut config = config::load();
    if let Some(days) = retention {
        config.log_retention_days = days;
        config::save(&config)?;
        println!("keeping {days} day(s) of logs");
    }
    let removed = log::prune(config.log_retention_days)?;
    if removed > 0 {
        println!("removed {removed} log file(s) past the {} day window", config.log_retention_days);
    }
    for line in log::search(search, lines)? {
        println!("{line}");
    }
    Ok(())
}

async fn status() -> Result<()> {
    let info = account::info().await?;
    println!(
        "{}: {:.1} / {:.1} GiB used",
        info.username,
        info.used_bytes as f64 / 1_073_741_824.0,
        info.total_bytes as f64 / 1_073_741_824.0
    );
    Ok(())
}

async fn do_logout() -> Result<()> {
    account::logout().await?;
    println!("logged out");
    Ok(())
}

async fn ls(path: &str) -> Result<()> {
    let (mut drive, before) = account::open_drive().await?;
    let folder = drive.resolve(path).await?;
    for n in drive.list(&folder).await? {
        println!("{} {}", if n.is_folder { "d" } else { "-" }, n.name);
    }
    account::persist(drive.api, before).await
}

async fn get(remote: &str, local: Option<std::path::PathBuf>) -> Result<()> {
    let (mut drive, before) = account::open_drive().await?;
    let file = drive.resolve(remote).await?;
    let dest = local.unwrap_or_else(|| std::path::PathBuf::from(&file.name));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?);
    let n = drive.download(&file, &mut out).await?;
    std::io::Write::flush(&mut out)?;
    println!("{} bytes -> {}", n, dest.display());
    account::persist(drive.api, before).await
}

async fn put(local: &std::path::Path, remote_folder: &str) -> Result<()> {
    let (mut drive, before) = account::open_drive().await?;
    let folder = drive.resolve(remote_folder).await?;
    let name = local.file_name().and_then(|n| n.to_str()).context("local path has no file name")?;
    let existing = drive.list(&folder).await?.into_iter().find(|n| n.name == name && !n.is_folder);
    let meta = std::fs::metadata(local)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let mut file = std::fs::File::open(local)?;
    let (link, rev) = drive.upload(&folder, name, existing.as_ref(), &mut file, mtime).await?;
    println!("uploaded {} ({} bytes) link={link} revision={rev}", local.display(), meta.len());
    account::persist(drive.api, before).await
}

async fn share(remote: &str, copy: bool, password: Option<&str>, expires_days: Option<i64>) -> Result<()> {
    let remote = remote_path(remote)?;
    let (mut drive, before) = account::open_drive().await?;
    let (parent, node) = drive.resolve_with_parent(&remote).await?;
    let existing = drive.public_link(&node).await?.is_some();
    let url = drive.share(&parent, &node, password, expires_days).await?;
    println!("{url}");
    if copy {
        let copied = copy_to_clipboard(&url);
        daemon::notify(&if copied { format!("Link copied to the clipboard\n{url}") } else { format!("Link for {remote}\n{url}") });
    }
    if existing {
        println!("(this link already existed; its settings were left alone)");
    } else if let Some(p) = password.filter(|p| !p.is_empty()) {
        println!("password to send separately: {p}");
    }
    account::persist(drive.api, before).await
}

async fn unshare(remote: &str) -> Result<()> {
    let remote = remote_path(remote)?;
    let (mut drive, before) = account::open_drive().await?;
    let (_, node) = drive.resolve_with_parent(&remote).await?;
    match drive.unshare(&node).await? {
        0 => println!("{remote} has no public link"),
        n => log::info(&format!("removed {n} public link(s) from {remote}")),
    }
    account::persist(drive.api, before).await
}

/// Accepts either a remote path ("a/b.txt") or an absolute local path inside the
/// sync folder, so Dolphin can pass %f straight through.
fn remote_path(arg: &str) -> Result<String> {
    let path = std::path::Path::new(arg);
    if !path.is_absolute() {
        return Ok(arg.trim_start_matches('/').to_owned());
    }
    let root = sync::load_state()?.map(|s| s.root).context("no sync folder yet; run `kpdrive setup`")?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| anyhow!("{} is not inside the sync folder ({})", path.display(), root.display()))?;
    Ok(rel.to_string_lossy().into_owned())
}

/// Plasma keeps the clipboard alive through klipper once something owns it.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    for (cmd, args) in [("wl-copy", &[][..]), ("xclip", &["-selection", "clipboard"])] {
        let Ok(mut child) = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        else {
            continue;
        };
        let wrote = child.stdin.take().map(|mut i| i.write_all(text.as_bytes()).is_ok()).unwrap_or(false);
        let _ = child.wait();
        if wrote {
            return true;
        }
    }
    false
}

async fn mkdir(remote: &str) -> Result<()> {
    let (mut drive, before) = account::open_drive().await?;
    let (parent, name) = remote.trim_end_matches('/').rsplit_once('/').unwrap_or(("", remote));
    let parent = drive.resolve(parent).await?;
    let node = drive.create_folder(&parent, name).await?;
    println!("created folder {} id={}", node.name, node.id);
    account::persist(drive.api, before).await
}

fn resolve_root(state: &mut sync::State, root: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(root) = root {
        state.root = root;
    }
    if state.root.as_os_str().is_empty() {
        state.root = std::path::PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join("ProtonDrive");
    }
    Ok(())
}

async fn setup(root: Option<std::path::PathBuf>) -> Result<()> {
    let mut state = sync::load_state()?.unwrap_or_default();
    resolve_root(&mut state, root)?;
    std::fs::create_dir_all(&state.root)?;
    sync::save_state(&state)?;
    println!("folder: {}", state.root.display());
    if setup::places_entry(&state.root)? {
        println!("added Proton Drive to Dolphin's Places");
    }
    println!("autostart: {}", setup::autostart()?.display());
    println!("Dolphin menu: {}", setup::servicemenu()?.display());
    println!("launcher: {}", setup::launcher()?.display());
    if account::load().await?.is_none() {
        println!("not logged in yet: run `kpdrive login`");
    }
    println!("start now with: kpdrive sync --watch   (Dolphin overlay icons: see dolphin-overlay/README.md)");
    Ok(())
}

async fn photos(dest: Option<std::path::PathBuf>) -> Result<()> {
    let mut state = photos::load_state()?.unwrap_or_default();
    if let Some(dest) = dest {
        state.dest = dest;
    }
    if state.dest.as_os_str().is_empty() {
        state.dest = photos::default_dest()?;
    }
    photos::check_dest(&state.dest, sync::load_state()?.map(|s| s.root).as_deref())?;

    let (mut drive, before) = account::open_drive().await?;
    match photos::run(&mut drive, &mut state).await? {
        None => println!("this account has no Proton Photos library"),
        Some(0) => println!("photos up to date in {}", state.dest.display()),
        Some(n) => println!("{n} photo(s) into {}", state.dest.display()),
    }
    photos::save_state(&state)?;
    account::persist(drive.api, before).await
}

async fn sync(root: Option<std::path::PathBuf>, watch: bool, force: bool) -> Result<()> {
    let mut state = sync::load_state()?.unwrap_or_default();
    resolve_root(&mut state, root)?;
    let (mut drive, before) = account::open_drive().await?;
    if watch {
        let mut last = before;
        return daemon::run(drive, state, move |d| {
            if let Some(s) = d.api.session.clone().filter(|s| *s != last) {
                // Wallet writes are async; block briefly on a small runtime-free path.
                let s2 = s.clone();
                tokio::spawn(async move {
                    if let Err(e) = account::save(&s2).await {
                        eprintln!("could not store refreshed session: {e:#}");
                    }
                });
                last = s;
            }
        })
        .await;
    }
    match sync::run(&mut drive, &mut state, force).await? {
        Some(_) => println!("synced to {}", state.root.display()),
        None => {}
    }
    account::persist(drive.api, before).await
}








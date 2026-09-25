
use kpdrive::account::{self, Account};
use kpdrive::{config, daemon, i18n, log, photos, setup, sync};
use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Proton Drive sync client for KDE Plasma")]
struct Cli {
    /// The account to act on, by username or the start of one. Needed once
    /// there are several; commands given a path in a sync folder find it.
    #[arg(short, long, global = true)]
    account: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in via the browser and store the session in KWallet. Adds the
    /// account, or signs an account already here back in.
    Login,
    /// Show what the daemon is doing and each account's storage.
    Status,
    /// List the accounts: who is signed in, and where each one syncs.
    Accounts,
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
    /// End the session server-side and forget it. The account keeps its
    /// folder, ready for the next sign-in.
    Logout {
        /// Also forget the account and what was synced for it. The files in
        /// its folders stay where they are.
        #[arg(long)]
        remove: bool,
    },
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
    /// Pause syncing, photos included, until `resume`. Survives restarts.
    Pause,
    /// Resume syncing after `pause`. Networks set to pause still do.
    Resume,
    /// Upload the photo ingestion folder into Proton Photos once, then trash
    /// (or with photos_ingestion_perm_rm, delete) each file that went up.
    Ingest {
        /// The folder to ingest; remembered for the daemon.
        #[arg(long)]
        folder: Option<std::path::PathBuf>,
    },
    /// Sync Drive with the local folder (two-way), for every account or the
    /// one given. --watch keeps running with a tray icon, for every account.
    Sync {
        /// Local folder; remembered after the first run. Default: ~/ProtonDrive
        #[arg(long)]
        root: Option<std::path::PathBuf>,
        /// Keep running and re-sync whenever the volume changes.
        #[arg(long)]
        watch: bool,
        /// Also bring down the Proton Photos timeline, every half hour while
        /// watching. Photos are download only: nothing is ever uploaded to
        /// them or removed from them.
        #[arg(long)]
        photos: bool,
        /// Forget what was synced and re-adopt this folder for the account.
        /// Files are compared by content, so nothing is lost.
        #[arg(long)]
        adopt: bool,
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
    /// Move a remote file or folder to the trash.
    Rm {
        remote: String,
        /// Do not ask first. Needed when there is no terminal to ask on.
        #[arg(short, long)]
        force: bool,
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
    i18n::init();
    // Before anything reads an account: a single-account setup is moved over
    // to the per-account layout the first time a new version runs.
    if let Err(e) = account::migrate().await {
        eprintln!("could not move the stored account over to the new layout: {e:#}");
    }
    let cli = Cli::parse();
    let who = cli.account.as_deref();
    match cli.cmd {
        Cmd::Login => login().await,
        Cmd::Status => status().await,
        Cmd::Accounts => accounts().await,
        Cmd::Logs { search, lines, retention } => logs(&search, lines, retention),
        Cmd::Logout { remove } => do_logout(&account::select(who)?, remove).await,
        Cmd::Ls { path } => ls(&account::select(who)?, &path).await,
        Cmd::Get { remote, local } => get(&account::select(who)?, &remote, local).await,
        Cmd::Sync { root, watch, force, adopt, photos } => sync(who, root, watch, force, adopt, photos).await,
        Cmd::Photos { dest } => photos(&account::select(who)?, dest).await,
        Cmd::Ingest { folder } => ingest(&account::select(who)?, folder).await,
        Cmd::Pause => {
            kpdrive::pause::set(true)?;
            println!("paused; `kpdrive resume` picks up where it left off");
            Ok(())
        }
        Cmd::Resume => {
            kpdrive::pause::set(false)?;
            match kpdrive::pause::reason() {
                Some(why) => println!("{why}"),
                None => println!("resumed"),
            }
            Ok(())
        }
        Cmd::Put { local, remote_folder } => put(&account::select(who)?, &local, &remote_folder).await,
        Cmd::Setup { root } => setup(&account::select(who)?, root).await,
        Cmd::Mkdir { remote } => mkdir(&account::select(who)?, &remote).await,
        Cmd::Rm { remote, force } => rm(who, &remote, force).await,
        Cmd::Share { remote, copy, password, expires_days } => share(who, &remote, copy, password.as_deref(), expires_days).await,
        Cmd::Unshare { remote } => unshare(who, &remote).await,
    }
}

async fn login() -> Result<()> {
    let (account, new) = account::login(|url, code| {
        println!("Sign in in your browser. Confirm this code there: {code}\n{url}");
        if daemon::spawn_detached(std::process::Command::new("xdg-open").arg(url)).is_err() {
            eprintln!("could not run xdg-open; open the URL above manually");
        }
        // Fire-and-forget: kdialog would block on OK, and the browser is what matters.
        let _ = daemon::spawn_detached(
            std::process::Command::new("kdialog").args(["--title", "kpdrive", "--passivepopup", &format!("Confirm code {code} in your browser"), "30"]),
        );
    })
    .await?;
    println!("logged in as {}", account.username);
    if new {
        if let Some(folder) = account.sync_folder() {
            println!("syncing to {}; to sync elsewhere: kpdrive setup --account {} --root DIR", folder.display(), account.username);
        }
    }
    // A running daemon starts on a new account, or takes up the new session.
    daemon::poke();
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
    // The daemon first: when the session is gone, that is what explains the
    // account line failing, and it is the same sentence the window shows.
    let report = daemon::ask();
    let accounts = account::all();
    if accounts.is_empty() {
        println!("{}", report.sentence(None));
        println!("not logged in: run `kpdrive login`");
        return Ok(());
    }
    for (i, account) in accounts.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}: {}", account.username, report.sentence(Some(&account.username)));
        match account.info().await {
            Ok(info) => println!(
                "  {:.1} / {:.1} GiB used",
                info.used_bytes as f64 / 1_073_741_824.0,
                info.total_bytes as f64 / 1_073_741_824.0
            ),
            Err(e) => println!("  {e:#}"),
        }
    }
    Ok(())
}

async fn accounts() -> Result<()> {
    let accounts = account::all();
    if accounts.is_empty() {
        println!("no accounts: run `kpdrive login`");
    }
    for account in accounts {
        let signed_in = matches!(account.session().await, Ok(Some(_)));
        let folder = account.sync_folder().map(|f| f.display().to_string()).unwrap_or_else(|| "no folder yet".into());
        println!("{}\t{}\t{folder}", account.username, if signed_in { "signed in" } else { "signed out" });
    }
    Ok(())
}

async fn do_logout(account: &Account, remove: bool) -> Result<()> {
    if remove {
        let folders = account.folders();
        account.remove().await?;
        println!("removed {}", account.username);
        for folder in folders.iter().filter(|f| f.exists()) {
            println!("left in place: {}", folder.display());
        }
    } else {
        account.logout().await?;
        println!("logged out of {}", account.username);
    }
    daemon::poke();
    Ok(())
}

async fn ls(account: &Account, path: &str) -> Result<()> {
    let (drive, before) = account.open_drive().await?;
    let folder = drive.resolve(path).await?;
    for n in drive.list(&folder).await? {
        println!("{} {}", if n.is_folder { "d" } else { "-" }, n.name);
    }
    account.persist(drive.api, before).await
}

async fn get(account: &Account, remote: &str, local: Option<std::path::PathBuf>) -> Result<()> {
    let (drive, before) = account.open_drive().await?;
    let file = drive.resolve(remote).await?;
    let dest = local.unwrap_or_else(|| std::path::PathBuf::from(&file.name));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?);
    let n = drive.download(&file, &mut out).await?;
    std::io::Write::flush(&mut out)?;
    println!("{} bytes -> {}", n, dest.display());
    account.persist(drive.api, before).await
}

async fn put(account: &Account, local: &std::path::Path, remote_folder: &str) -> Result<()> {
    let (drive, before) = account.open_drive().await?;
    let folder = drive.resolve(remote_folder).await?;
    let name = local.file_name().and_then(|n| n.to_str()).context("local path has no file name")?;
    let existing = drive.list(&folder).await?.into_iter().find(|n| n.name == name && !n.is_folder);
    let meta = std::fs::metadata(local)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let mut file = std::fs::File::open(local)?;
    let (link, rev) = drive.upload(&folder, name, existing.as_ref(), &mut file, mtime).await?;
    println!("uploaded {} ({} bytes) link={link} revision={rev}", local.display(), meta.len());
    account.persist(drive.api, before).await
}

async fn share(who: Option<&str>, remote: &str, copy: bool, password: Option<&str>, expires_days: Option<i64>) -> Result<()> {
    let (account, remote) = remote_path(who, remote)?;
    let (drive, before) = account.open_drive().await?;
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
    account.persist(drive.api, before).await
}

async fn unshare(who: Option<&str>, remote: &str) -> Result<()> {
    let (account, remote) = remote_path(who, remote)?;
    let (drive, before) = account.open_drive().await?;
    let (_, node) = drive.resolve_with_parent(&remote).await?;
    match drive.unshare(&node).await? {
        0 => println!("{remote} has no public link"),
        n => log::info(&format!("removed {n} public link(s) from {remote}")),
    }
    account.persist(drive.api, before).await
}

/// Accepts either a remote path ("a/b.txt") or an absolute local path inside a
/// sync folder, so Dolphin can pass %f straight through. A local path says
/// which account it is for by the folder it is in.
fn remote_path(who: Option<&str>, arg: &str) -> Result<(Account, String)> {
    let path = std::path::Path::new(arg);
    if !path.is_absolute() {
        return Ok((account::select(who)?, arg.trim_start_matches('/').to_owned()));
    }
    let account = match who {
        Some(_) => account::select(who)?,
        None => account::for_path(path).ok_or_else(|| anyhow!("{} is not inside a sync folder", path.display()))?,
    };
    let root = account.sync_folder().context("no sync folder yet; run `kpdrive setup`")?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| anyhow!("{} is not inside the sync folder of {} ({})", path.display(), account.username, root.display()))?;
    Ok((account, rel.to_string_lossy().into_owned()))
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

async fn rm(who: Option<&str>, remote: &str, force: bool) -> Result<()> {
    let (account, remote) = remote_path(who, remote)?;
    let (drive, before) = account.open_drive().await?;
    let (_, node) = drive.resolve_with_parent(&remote).await?;
    if !force {
        use std::io::IsTerminal;
        // A folder takes everything under it, which is worth saying before
        // rather than after.
        let what = match node.is_folder {
            true => format!("{remote} and everything in it"),
            false => remote.clone(),
        };
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("{remote} was left alone: there is no terminal to confirm on, so pass --force");
        }
        if !confirm(&format!("Move {what} to the Proton Drive trash?"))? {
            println!("left alone");
            return Ok(());
        }
    }
    drive.trash(std::slice::from_ref(&node.id)).await?;
    log::info(&format!("trashed {remote}"));
    account.persist(drive.api, before).await
}

async fn mkdir(account: &Account, remote: &str) -> Result<()> {
    let (drive, before) = account.open_drive().await?;
    let (parent, name) = remote.trim_end_matches('/').rsplit_once('/').unwrap_or(("", remote));
    let parent = drive.resolve(parent).await?;
    let node = drive.create_folder(&parent, name).await?;
    println!("created folder {} id={}", node.name, node.id);
    account.persist(drive.api, before).await
}

/// Where to sync: an explicit `--root` wins, then the configured folder, then
/// `~/ProtonDrive`. A folder set by an older version lives in the sync state
/// instead, so it is adopted into the config on first sight.
/// A yes/no question, where anything but yes means no. Only call it with a
/// terminal to ask on: the caller decides what no terminal means.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{BufRead, Write};
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Puts the occupied-folder choice to the user, in the same words the window
/// uses. Anything that is not a terminal (the autostarted daemon, a script)
/// takes the answer that changes nothing.
fn ask_about(state: &sync::State, root: &std::path::Path) -> Result<sync::Occupied> {
    use std::io::{BufRead, IsTerminal, Write};
    let Some(question) = sync::folder_question(state, root) else { return Ok(sync::Occupied::Merge) };
    println!("{question}");
    if !std::io::stdin().is_terminal() {
        println!("{}", sync::Occupied::Merge.label());
        return Ok(sync::Occupied::Merge);
    }
    let mut line = String::new();
    loop {
        print!("[m] {}\n[r] {}\nm/r? ", sync::Occupied::Merge.label(), sync::Occupied::Rename.label());
        std::io::stdout().flush()?;
        line.clear();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            return Ok(sync::Occupied::Merge); // stdin closed
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "m" | "merge" => return Ok(sync::Occupied::Merge),
            "r" | "rename" | "move" => return Ok(sync::Occupied::Rename),
            _ => println!("answer m or r."),
        }
    }
}

fn resolve_root(account: &Account, state: &mut sync::State, root: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(root) = root {
        let choice = ask_about(state, &root)?;
        println!("{}", sync::set_folder(account, state, root, choice)?);
        return Ok(());
    }
    match account.sync_folder() {
        Some(folder) => state.root = folder,
        None => {
            if state.root.as_os_str().is_empty() {
                state.root = config::default_sync_folder()?;
            }
            account.check_folder(&state.root)?;
            let root = state.root.clone();
            account.update(|a| a.sync_folder = Some(root))?;
        }
    }
    // The remembered folder can have gained files of its own, or be one the
    // user filled before kpdrive ever ran.
    if ask_about(state, &state.root.clone())? == sync::Occupied::Rename {
        println!("{}", sync::move_aside(&state.root)?);
        state.untrack();
        sync::save_state(state)?;
    }
    Ok(())
}

async fn setup(account: &Account, root: Option<std::path::PathBuf>) -> Result<()> {
    let mut state = sync::open_state(account)?;
    resolve_root(account, &mut state, root)?;
    std::fs::create_dir_all(&state.root)?;
    sync::save_state(&state)?;
    println!("folder: {}", state.root.display());
    match setup::ignore_template(&state.root)? {
        true => println!("ignore file: {}", state.root.join(sync::IGNORE_FILE).display()),
        false => println!("ignore file: {} (kept)", state.root.join(sync::IGNORE_FILE).display()),
    }
    // The first account is plain "Proton Drive"; the others are told apart.
    let title = match account::all().first() == Some(account) {
        true => "Proton Drive".to_owned(),
        false => format!("Proton Drive ({})", account.username),
    };
    if setup::places_entry(&state.root, &title)? {
        println!("added {title} to Dolphin's Places");
    }
    println!("autostart: {}", setup::autostart()?.display());
    println!("Dolphin menu: {}", setup::servicemenu()?.display());
    println!("launcher: {}", setup::launcher()?.display());
    if account.session().await?.is_none() {
        println!("{} is signed out: run `kpdrive login`", account.username);
    }
    println!("start now with: kpdrive sync --watch   (Dolphin overlay icons: see dolphin-overlay/README.md)");
    Ok(())
}

async fn photos(account: &Account, dest: Option<std::path::PathBuf>) -> Result<()> {
    // A destination given on the command line is remembered for next time.
    if let Some(dest) = dest {
        account.check_folder(&dest)?;
        photos::set_dest(account, dest)?;
    }
    let (drive, before) = account.open_drive().await?;
    match log::for_account(&account.username, photos::pass(&drive, account)).await? {
        None => println!("this account has no Proton Photos library"),
        Some((0, dest)) => println!("photos up to date in {}", dest.display()),
        Some((n, dest)) => println!("{n} photo(s) into {}", dest.display()),
    }
    account.persist(drive.api, before).await
}

async fn ingest(account: &Account, folder: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(folder) = folder {
        account.check_folder(&folder)?;
        account.update(|a| a.photos_ingestion_folder = Some(folder))?;
    }
    let Some(folder) = account.ingest_folder() else {
        anyhow::bail!("no ingestion folder for {}: pass --folder, or set photos_ingestion_folder", account.username);
    };
    // Two passes over one folder would upload the same file twice.
    if daemon::ask().running {
        println!("the daemon is running and ingests {} on its own", folder.display());
        return Ok(());
    }
    let (drive, before) = account.open_drive().await?;
    // A file goes up once two looks agree on it: the first look only notes
    // what is there, so a copy still in progress is left alone.
    let mut seen = kpdrive::ingest::Seen::default();
    let n = log::for_account(&account.username, async {
        kpdrive::ingest::pass(&drive, account, &mut seen).await?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        kpdrive::ingest::pass(&drive, account, &mut seen).await
    })
    .await?;
    println!("{n} photo(s) uploaded from {}", folder.display());
    account.persist(drive.api, before).await
}

async fn sync(who: Option<&str>, root: Option<std::path::PathBuf>, watch: bool, force: bool, adopt: bool, photos: bool) -> Result<()> {
    // A folder or a re-adoption is about one account, so it has to be clear
    // which; a plain sync is about all of them.
    let accounts = match (who, root.is_some() || adopt) {
        (None, false) => account::all(),
        _ => vec![account::select(who)?],
    };
    let mut root = root;
    for account in &accounts {
        let mut state = sync::open_state(account)?;
        resolve_root(account, &mut state, root.take())?;
        if adopt {
            // Whatever was tracked belonged to another account, or to nobody
            // we can name. Contents decide what is already there.
            state.untrack();
            sync::save_state(&state)?;
            println!("re-adopting {} for {}", state.root.display(), account.username);
        }
        if watch {
            continue;
        }
        let (mut drive, before) = account.open_drive().await?;
        if log::for_account(&account.username, sync::run(&mut drive, &mut state, force)).await?.is_some() {
            println!("{} synced to {}", account.username, state.root.display());
        }
        account.persist(drive.api, before).await?;
    }
    if watch {
        return daemon::run(photos).await;
    }
    if accounts.is_empty() {
        anyhow::bail!("not logged in, run `kpdrive login`");
    }
    Ok(())
}

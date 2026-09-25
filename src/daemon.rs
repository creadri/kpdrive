//! Long-running mode: periodic sync, Plasma tray icon, and a Unix socket that
//! the Dolphin overlay plugin (and anyone else) can ask for per-file status.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::account::Account;
use crate::sync::{self, Entry, State, local_matches};

const POLL: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a burst of filesystem events must be quiet before a pass starts:
/// an editor's write-temp-then-rename, or a lock file, then counts as one edit.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);
/// A burst that never goes quiet still gets a pass this often.
const DEBOUNCE_CAP: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the photos timeline is looked at, when that is switched on.
/// Photos arrive from a phone at their own pace, and the listing is a couple
/// of calls, so this is slower than the folder poll rather than faster.
const PHOTOS_EVERY: std::time::Duration = std::time::Duration::from_secs(1800);

/// Even with inotify, sweep the folder now and then: network mounts and watch
/// limits can lose events.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

/// What the tray, the socket and the window see of one account; refreshed
/// after every pass.
#[derive(Default)]
struct Snapshot {
    root: PathBuf,
    entries: HashMap<PathBuf, Entry>,
    syncing: bool,
    last_error: Option<String>,
    /// No usable session: waiting for a sign-in rather than failing every pass.
    signed_out: bool,
    /// Proton unreachable: retrying, and saying so rather than logging it.
    offline: bool,
    /// When the last pass finished, seconds since the epoch.
    last_sync: Option<i64>,
    /// The photos folder, and every downloaded photo and folder in it,
    /// relative to it.
    photos_root: Option<PathBuf>,
    photos: HashSet<PathBuf>,
}

impl Snapshot {
    fn json(&self, username: &str) -> serde_json::Value {
        serde_json::json!({
            "username": username,
            "root": self.root.display().to_string(),
            "items": self.entries.len(),
            "syncing": self.syncing,
            "signedOut": self.signed_out,
            "offline": self.offline,
            "lastSync": self.last_sync,
            "error": self.last_error,
        })
    }

    /// One line for the tray's tooltip.
    fn sentence(&self) -> String {
        match (self.signed_out, &self.last_error, self.syncing) {
            (true, ..) => crate::i18n::t("Signed out. Sign in from the account window.").into(),
            (_, Some(e), _) => e.clone(),
            (_, None, true) => crate::i18n::t("Syncing…").into(),
            (_, None, false) => crate::i18n::fill(
                crate::i18n::tn("{n} item in sync", "{n} items in sync", self.entries.len() as u64),
                &[("n", &self.entries.len().to_string())],
            ),
        }
    }
}

/// Every account's snapshot, in the order the accounts were added, and why
/// syncing is paused, when it is.
#[derive(Default)]
struct Board {
    accounts: Vec<(String, Snapshot)>,
    paused: Option<String>,
}

impl Board {
    /// Changes an account's snapshot. One the supervisor has since dropped
    /// stays dropped: a loop finishing its last pass must not bring it back.
    fn update(&mut self, username: &str, change: impl FnOnce(&mut Snapshot)) {
        if let Some((_, s)) = self.accounts.iter_mut().find(|(u, _)| u == username) {
            change(s);
        }
    }

    /// One line of JSON for the window. The top-level fields sum the accounts
    /// up, as a window from before there were several expects them.
    fn report(&self) -> String {
        let snaps = || self.accounts.iter().map(|(_, s)| s);
        serde_json::json!({
            "root": snaps().next().map(|s| s.root.display().to_string()).unwrap_or_default(),
            "items": snaps().map(|s| s.entries.len()).sum::<usize>(),
            "syncing": snaps().any(|s| s.syncing),
            "signedOut": self.accounts.is_empty() || snaps().any(|s| s.signed_out),
            "offline": snaps().any(|s| s.offline),
            "lastSync": snaps().filter_map(|s| s.last_sync).max(),
            "error": snaps().find_map(|s| s.last_error.clone()),
            "paused": self.paused,
            "accounts": self.accounts.iter().map(|(u, s)| s.json(u)).collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// Per-path status for overlays, from whichever account the path is in.
    fn status(&self, path: &Path) -> &'static str {
        self.accounts.iter().map(|(_, s)| status(s, path)).find(|s| *s != "NONE").unwrap_or("NONE")
    }

    /// Every folder the overlays should look at, as a JSON array.
    fn folders(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        for (_, s) in &self.accounts {
            if !s.root.as_os_str().is_empty() {
                out.push(s.root.display().to_string());
            }
            out.extend(s.photos_root.as_ref().map(|p| p.display().to_string()));
        }
        serde_json::Value::from(out).to_string()
    }
}

type Shared = Arc<RwLock<Board>>;

enum Cmd {
    /// Sync now: every account, or the one named.
    Sync(Option<String>),
    Quit,
}

struct Tray {
    board: Shared,
    cmds: mpsc::UnboundedSender<Cmd>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "kpdrive".into()
    }
    /// Clicking the icon opens the account window, which is what a tray icon
    /// for a sync client is expected to do. The protocol has no notion of a
    /// double click: the desktop decides what counts, and sends this.
    fn activate(&mut self, _x: i32, _y: i32) {
        open_window();
    }
    fn title(&self) -> String {
        "Proton Drive".into()
    }
    fn icon_name(&self) -> String {
        "folder-cloud".into()
    }
    fn overlay_icon_name(&self) -> String {
        let b = self.board.read().expect("board lock");
        if b.accounts.is_empty() || b.accounts.iter().any(|(_, s)| s.last_error.is_some() || s.signed_out) {
            "emblem-error".into()
        } else if b.paused.is_some() {
            "media-playback-pause".into()
        } else {
            String::new()
        }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let b = self.board.read().expect("board lock");
        let description = match (&b.paused, b.accounts.as_slice()) {
            (Some(why), _) => why.clone(),
            (None, []) => crate::i18n::t("Signed out. Sign in from the account window.").into(),
            (None, [(_, only)]) => only.sentence(),
            (None, many) => many.iter().map(|(u, s)| format!("{u}: {}", s.sentence())).collect::<Vec<_>>().join("\n"),
        };
        ksni::ToolTip { title: "Proton Drive".into(), description, ..Default::default() }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let (roots, paused): (Vec<(String, PathBuf)>, bool) = {
            let b = self.board.read().expect("board lock");
            (b.accounts.iter().map(|(u, s)| (u.clone(), s.root.clone())).collect(), b.paused.is_some())
        };
        let open = |label: String, root: PathBuf| -> MenuItem<Self> {
            StandardItem {
                label,
                icon_name: "folder-open".into(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = spawn_detached(std::process::Command::new("xdg-open").arg(&root));
                }),
                ..Default::default()
            }
            .into()
        };
        // One account opens straight away; several are listed by name.
        let folder = match roots.len() {
            0 | 1 => open(crate::i18n::t("Open folder").into(), roots.first().map(|(_, r)| r.clone()).unwrap_or_default()),
            _ => SubMenu {
                label: crate::i18n::t("Open folder").into(),
                icon_name: "folder-open".into(),
                submenu: roots.into_iter().map(|(u, r)| open(u, r)).collect(),
                ..Default::default()
            }
            .into(),
        };
        vec![
            folder,
            StandardItem {
                label: crate::i18n::t("Account and logs…").into(),
                icon_name: "user-identity".into(),
                activate: Box::new(|_: &mut Self| open_window()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: crate::i18n::t("Sync now").into(),
                icon_name: "view-refresh".into(),
                enabled: !paused,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmds.send(Cmd::Sync(None));
                }),
                ..Default::default()
            }
            .into(),
            // Only the manual pause is toggled here: a network or power pause
            // lifts itself when that changes.
            CheckmarkItem {
                label: crate::i18n::t("Pause sync").into(),
                checked: crate::config::load().sync_paused,
                activate: Box::new(|_: &mut Self| {
                    let paused = crate::config::load().sync_paused;
                    if let Err(e) = crate::pause::set(!paused) {
                        crate::log::error(&format!("cannot save the pause: {e:#}"));
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: crate::i18n::t("Quit").into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmds.send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Starts the account window, unless one is already open. Clicking the icon
/// half a dozen times should not leave half a dozen windows behind; a window
/// that is open but buried stays where it is, because raising another
/// process's window is not something Wayland allows.
fn open_window() {
    let exe = match crate::setup::ui_binary() {
        Ok(exe) => exe,
        Err(e) => return crate::log::error(&format!("cannot locate the window: {e:#}")),
    };
    if running(&exe) {
        return;
    }
    if let Err(e) = spawn_detached(&mut std::process::Command::new(&exe)) {
        crate::log::error(&format!("cannot start {}: {e}", exe.display()));
    }
}

/// Whether a process of this program is already running. /proc says so
/// outright, where a lock file would have to be cleaned up after a crash.
fn running(exe: &Path) -> bool {
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return false;
    };
    // The kernel keeps only the first 15 bytes of a name in comm.
    let short = &name[..name.len().min(15)];
    let Ok(entries) = std::fs::read_dir("/proc") else { return false };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
            && std::fs::read_to_string(e.path().join("comm")).map(|c| c.trim() == short).unwrap_or(false)
            && !std::fs::read_to_string(e.path().join("stat")).is_ok_and(|s| zombie(&s))
    })
}

/// Whether a `/proc/<pid>/stat` line is a process that has exited and not
/// been reaped. Such a process keeps its name in /proc but is not running.
fn zombie(stat: &str) -> bool {
    // `pid (comm) state …`; comm may itself contain spaces and parentheses.
    stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
}

/// Starts a program the daemon does not wait for, and reaps it once it exits.
/// Dropping a `Child` does not: every notification and window would otherwise
/// stay behind as a zombie for as long as the daemon runs.
pub fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<u32> {
    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || child.wait());
    Ok(pid)
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("kpdrive.sock")
}


/// Reads what the account's photo download has written, for the overlays.
/// Only at startup and after a photo pass: that is the only time it changes.
fn refresh_photos(board: &Shared, account: &Account) {
    let Ok(Some(state)) = crate::photos::load_state(account) else { return };
    let mut photos = HashSet::new();
    for entry in state.photos.values() {
        photos.extend(entry.path.ancestors().map(Path::to_path_buf));
    }
    board.write().expect("board lock").update(&account.username, |s| {
        s.photos_root = Some(state.dest).filter(|d| !d.as_os_str().is_empty());
        s.photos = photos;
    });
}

/// Per-path status for overlays: OK, SYNC (pending), NONE (not ours).
fn status(snap: &Snapshot, path: &Path) -> &'static str {
    let Some(rel) = path.strip_prefix(&snap.root).ok().filter(|_| !snap.root.as_os_str().is_empty()) else {
        // Photos are download only, so a photo is either here or not ours.
        return match snap.photos_root.as_deref().map(|r| path.strip_prefix(r)) {
            Some(Ok(rel)) if snap.photos.contains(rel) => "OK",
            _ => "NONE",
        };
    };
    match snap.entries.get(rel) {
        Some(e) if e.is_folder => "OK",
        Some(e) => match std::fs::metadata(path) {
            Ok(m) if local_matches(&m, e) => "OK",
            _ => "SYNC",
        },
        None if rel.as_os_str().is_empty() => "OK",
        None => "SYNC",
    }
}

async fn serve_socket(board: Shared, cmds: mpsc::UnboundedSender<Cmd>) -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let board = board.clone();
        let cmds = cmds.clone();
        tokio::spawn(async move {
            let (r, mut w) = stream.into_split();
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply = {
                    let b = board.read().expect("board lock");
                    let first = b.accounts.first().map(|(_, s)| s);
                    match line.split_once(' ') {
                        Some(("STATUS", p)) => b.status(Path::new(p)).to_string(),
                        Some(("SYNC", who)) => {
                            let _ = cmds.send(Cmd::Sync(Some(who.to_owned())));
                            "OK".into()
                        }
                        // The first account's, for overlay plugins from before
                        // there were several; newer ones ask for FOLDERS.
                        _ if line == "ROOT" => first.map(|s| s.root.display().to_string()).unwrap_or_default(),
                        _ if line == "PHOTOS" => {
                            first.and_then(|s| s.photos_root.as_ref()).map(|p| p.display().to_string()).unwrap_or_default()
                        }
                        _ if line == "FOLDERS" => b.folders(),
                        _ if line == "STATE" => b.report(),
                        _ if line == "SYNC" => {
                            let _ = cmds.send(Cmd::Sync(None));
                            "OK".into()
                        }
                        _ if line == "QUIT" => {
                            let _ = cmds.send(Cmd::Quit);
                            "OK".into()
                        }
                        _ => "ERR".into(),
                    }
                };
                if w.write_all(format!("{reply}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

/// A connection that drops and comes back within this long is one episode,
/// not two. Without it a flapping link reports every dip.
const FLAP: std::time::Duration = std::time::Duration::from_secs(300);

/// Tracks an outage so that losing the network costs one log line when it
/// goes and one when it returns, however many passes fail in between.
///
/// `now` is passed in rather than read, so the rules can be tested without
/// waiting for the clock.
#[derive(Default)]
struct Outage {
    since: Option<std::time::Instant>,
    /// Whether the user was told about the outage in hand. A dip that was not
    /// worth mentioning is not worth an all-clear either.
    reported: bool,
    last_report: Option<std::time::Instant>,
}

impl Outage {
    /// A pass failed because Proton could not be reached. Gives the line to
    /// log, the first time an outage is worth mentioning and never again.
    fn failing(&mut self, now: std::time::Instant, reason: &str) -> Option<String> {
        let since = *self.since.get_or_insert(now);
        if self.reported {
            return None;
        }
        // Straight after a report, a failure is the same flapping link. One
        // that lasts is worth its own line, once it is clear it will.
        let settling = self.last_report.map(|t| now.duration_since(t) < FLAP).unwrap_or(false);
        if settling && now.duration_since(since) < FLAP {
            return None;
        }
        self.reported = true;
        self.last_report = Some(now);
        // TRANSLATORS: {reason} is a sentence, e.g. "cannot reach Proton Drive: no route"
        Some(crate::i18n::fill(crate::i18n::t("{reason}; retrying"), &[("reason", reason)]))
    }

    /// A pass succeeded. Gives the all-clear, if the outage was mentioned.
    fn ended(&mut self, now: std::time::Instant) -> Option<String> {
        let since = self.since.take()?;
        if !std::mem::take(&mut self.reported) {
            return None;
        }
        self.last_report = Some(now);
        Some(crate::i18n::fill(
            crate::i18n::t("Proton Drive is reachable again, after {duration}"),
            &[("duration", &spell(now.duration_since(since)))],
        ))
    }
}

/// A rough duration, in the largest unit that still says something.
fn spell(d: std::time::Duration) -> String {
    use crate::i18n::{fill, tn};
    let secs = d.as_secs();
    let (template, n) = match secs {
        0..=90 => (tn("{n} second", "{n} seconds", secs), secs),
        91..=5400 => (tn("{n} minute", "{n} minutes", secs / 60), secs / 60),
        _ => (tn("{n} hour", "{n} hours", secs / 3600), secs / 3600),
    };
    fill(template, &[("n", &n.to_string())])
}


/// What a daemon is doing, as anything outside it sees it. `running: false`
/// is the answer when there is no daemon to ask.
#[derive(Default, Debug, Clone)]
pub struct Report {
    pub running: bool,
    /// Whether it answered the question. An older daemon holds the socket but
    /// does not know this command.
    pub answered: bool,
    /// Why syncing is paused, when it is.
    pub paused: Option<String>,
    /// Each account the daemon runs, in the order they were added.
    pub accounts: Vec<AccountReport>,
}

/// What the daemon is doing for one account.
#[derive(Default, Debug, Clone)]
pub struct AccountReport {
    /// Empty from a daemon older than accounts, which only ever had one.
    pub username: String,
    pub root: String,
    pub syncing: bool,
    pub signed_out: bool,
    /// Proton cannot be reached. Still trying, and nothing is wrong as such.
    pub offline: bool,
    pub items: usize,
    pub last_sync: Option<i64>,
    pub error: Option<String>,
}

impl Report {
    /// The daemon's account of `username`. A daemon older than accounts
    /// answers for whoever asks.
    pub fn account(&self, username: &str) -> Option<&AccountReport> {
        self.accounts.iter().find(|a| a.username.eq_ignore_ascii_case(username) || a.username.is_empty())
    }

    /// One sentence about `username`, or about the first account, worded
    /// once so the window and `kpdrive status` say the same thing about the
    /// same daemon.
    pub fn sentence(&self, username: Option<&str>) -> String {
        use crate::i18n::t;
        if !self.running {
            return t("The sync daemon is not running. Start it with: kpdrive sync --watch").into();
        }
        if !self.answered {
            return t("A sync daemon is running but is too old to say what it is doing. Restart it.").into();
        }
        if let Some(p) = &self.paused {
            return p.clone();
        }
        let account = match username {
            Some(u) => self.account(u),
            None => self.accounts.first(),
        };
        match account {
            Some(a) => a.sentence(),
            None => t("Signed out. Syncing resumes once you sign in.").into(),
        }
    }
}

impl AccountReport {
    fn sentence(&self) -> String {
        use crate::i18n::{fill, t, tn};
        if self.signed_out {
            return t("Signed out. Syncing resumes once you sign in.").into();
        }
        if let Some(e) = &self.error {
            let line = match self.offline {
                true => t("{reason}. Still trying."),
                false => t("Last sync failed: {reason}"),
            };
            return fill(line, &[("reason", e)]);
        }
        if self.syncing {
            return t("Syncing…").into();
        }
        let items = fill(
            tn("{n} item in sync", "{n} items in sync", self.items as u64),
            &[("n", &self.items.to_string())],
        );
        let ago = match self.last_sync.map(|t| now() - t) {
            None => return items,
            // TRANSLATORS: how long ago the last sync ran; fills {ago} below
            Some(secs) if secs < 90 => t("just now").into(),
            Some(secs) if secs < 5400 => {
                fill(tn("{n} minute ago", "{n} minutes ago", (secs / 60) as u64), &[("n", &(secs / 60).to_string())])
            }
            Some(secs) => {
                fill(tn("{n} hour ago", "{n} hours ago", (secs / 3600) as u64), &[("n", &(secs / 3600).to_string())])
            }
        };
        // TRANSLATORS: {items} is "5 items in sync", {ago} is "just now" or "3 minutes ago"
        fill(t("{items}, checked {ago}"), &[("items", &items), ("ago", &ago)])
    }

    fn parse(v: &serde_json::Value) -> Self {
        Self {
            username: v["username"].as_str().unwrap_or_default().to_owned(),
            root: v["root"].as_str().unwrap_or_default().to_owned(),
            syncing: v["syncing"].as_bool().unwrap_or(false),
            signed_out: v["signedOut"].as_bool().unwrap_or(false),
            offline: v["offline"].as_bool().unwrap_or(false),
            items: v["items"].as_u64().unwrap_or(0) as usize,
            last_sync: v["lastSync"].as_i64(),
            error: v["error"].as_str().map(str::to_owned),
        }
    }
}

/// Asks a running daemon what it is doing. A local socket with a short
/// timeout, so a wedged daemon reads as one that is not answering.
pub fn ask() -> Report {
    use std::io::{BufRead, BufReader, Write};
    let deadline = std::time::Duration::from_millis(300);
    let Ok(stream) = std::os::unix::net::UnixStream::connect(socket_path()) else {
        return Report::default();
    };
    // Answering the socket is what proves a daemon is there. A version that
    // does not know this command still counts as running.
    let mut report = Report { running: true, ..Default::default() };
    let spoke = || -> Option<serde_json::Value> {
        stream.set_read_timeout(Some(deadline)).ok()?;
        stream.set_write_timeout(Some(deadline)).ok()?;
        let mut writer = stream.try_clone().ok()?;
        writer.write_all(b"STATE\n").ok()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    };
    if let Some(v) = spoke() {
        report.answered = true;
        report.paused = v["paused"].as_str().map(str::to_owned);
        report.accounts = match v["accounts"].as_array() {
            Some(list) => list.iter().map(AccountReport::parse).collect(),
            // A daemon from before accounts: its one account is the top level.
            None => vec![AccountReport::parse(&v)],
        };
    }
    report
}

/// Tells a running daemon to sync now, which is also how it is nudged to pick
/// up an account or a session that has just changed. Silent when there is no
/// daemon.
pub fn poke() {
    send("SYNC\n");
}

/// As [`poke`], for one account.
pub fn poke_account(username: &str) {
    send(&format!("SYNC {username}\n"));
}

fn send(line: &str) {
    use std::io::Write;
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket_path()) {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(300)));
        let _ = stream.write_all(line.as_bytes());
    }
}

/// A desktop notification. Said on an account's behalf, it names the account
/// once there is more than one to tell apart.
pub fn notify(body: &str) {
    let title = match crate::log::account() {
        Some(a) if crate::config::load().accounts.len() > 1 => format!("Proton Drive ({a})"),
        _ => "Proton Drive".into(),
    };
    let _ = spawn_detached(std::process::Command::new("notify-send").args(["-a", "kpdrive", "-i", "folder-cloud", &title, body]));
}

/// What the daemon is able to do at all, as distinct from how the last pass
/// went. Both of these are waiting on something rather than failing.
#[derive(Clone, Copy, PartialEq)]
enum Health {
    Ok,
    /// No session to work with.
    SignedOut,
    /// Proton cannot be reached.
    Offline,
}

fn refresh(board: &Shared, username: &str, state: &State, syncing: bool, error: Option<String>, health: Health) {
    board.write().expect("board lock").update(username, |s| {
        s.root = state.root.clone();
        s.entries = state.nodes.values().map(|e| (e.path.clone(), e.clone())).collect();
        s.syncing = syncing;
        s.last_error = error;
        s.signed_out = health == Health::SignedOut;
        s.offline = health == Health::Offline;
    });
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Waits for whichever comes first: a command, a settled burst of filesystem
/// events, or the timer. `false` means Quit.
async fn wait_for_work(
    rx: &mut mpsc::UnboundedReceiver<Cmd>,
    fs_rx: &mut mpsc::UnboundedReceiver<()>,
    wait: std::time::Duration,
    force: &mut bool,
    fs_dirty: &mut bool,
) -> Result<bool> {
    tokio::select! {
        cmd = rx.recv() => match cmd {
            Some(Cmd::Sync(_)) => {
                *force = true;
                // Several clicks during one pass mean one forced pass, not several.
                while let Ok(Cmd::Sync(_)) = rx.try_recv() {}
            }
            Some(Cmd::Quit) | None => return Ok(false),
        },
        Some(()) = fs_rx.recv() => {
            // Debounce: wait for the burst to go quiet, but not forever.
            let started = std::time::Instant::now();
            while started.elapsed() < DEBOUNCE_CAP {
                match tokio::time::timeout(DEBOUNCE, fs_rx.recv()).await {
                    Ok(Some(())) => continue,
                    _ => break,
                }
            }
            *fs_dirty = true;
        }
        _ = tokio::time::sleep(wait) => {}
    }
    Ok(true)
}

/// Watches the sync folder; every relevant change sends one `()`. `None` when
/// the watch could not be set up, in which case the caller sweeps instead.
fn watch(root: &Path, changed: mpsc::UnboundedSender<()>) -> Option<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let root_owned = root.to_owned();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        // Reads and metadata-only events (atime) say nothing about content.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let relevant = event.paths.iter().any(|p| {
            p.starts_with(&root_owned)
                && !p.to_string_lossy().ends_with(sync::PART_SUFFIX)
                && !p.file_name().map(|n| n.to_string_lossy().starts_with(".kpdrive")).unwrap_or(false)
        });
        if relevant {
            let _ = changed.send(());
        }
    })
    .ok()?;
    if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
        crate::log::warn(&format!("cannot watch {}: {e}; falling back to periodic sweeps", root.display()));
        return None;
    }
    Some(watcher)
}


/// An account's loop, as the supervisor keeps track of it.
struct Running {
    account: Account,
    /// The sync folder it started with. A different one means a restart.
    folder: Option<PathBuf>,
    cmds: mpsc::UnboundedSender<Cmd>,
    task: tokio::task::JoinHandle<()>,
}

/// Runs until Quit: one loop per account, under one tray icon and one status
/// socket. The accounts are read again whenever the daemon is poked, so one
/// added or removed in the window starts or stops at once, and one whose
/// folder moved starts again from the new place.
///
/// `always_photos` brings the photos timeline down for every account, whatever
/// its setting says.
pub async fn run(always_photos: bool) -> Result<()> {
    tokio::task::LocalSet::new().run_until(supervise(always_photos)).await
}

async fn supervise(always_photos: bool) -> Result<()> {
    let board: Shared = Arc::default();
    let (tx, mut rx) = mpsc::unbounded_channel();
    {
        let (board, tx) = (board.clone(), tx.clone());
        tokio::spawn(async move {
            if let Err(e) = serve_socket(board, tx).await {
                // Without it the overlay icons and the window learn nothing.
                crate::log::error(&format!("status socket unavailable: {e:#}"));
            }
        });
    }
    let tray = match ksni::TrayMethods::spawn(Tray { board: board.clone(), cmds: tx.clone() }).await {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("tray unavailable: {e}");
            None
        }
    };

    // Opens this run in the log; the window shows everything after it.
    crate::log::mark("daemon started");
    let mut loops: Vec<Running> = Vec::new();
    loop {
        reconcile(&mut loops, &board, &tray, always_photos).await;
        // Cheap: a directory listing.
        if let Err(e) = crate::log::prune(crate::config::load().log_retention_days) {
            crate::log::error(&format!("log retention: {e:#}"));
        }
        let cmd = tokio::select! {
            cmd = rx.recv() => cmd,
            _ = tokio::time::sleep(POLL) => continue,
        };
        match cmd {
            Some(Cmd::Sync(target)) => {
                // A poke is also how a new account or session is announced.
                reconcile(&mut loops, &board, &tray, always_photos).await;
                for r in &loops {
                    if target.as_deref().is_none_or(|t| r.account.username.eq_ignore_ascii_case(t)) {
                        let _ = r.cmds.send(Cmd::Sync(None));
                    }
                }
            }
            Some(Cmd::Quit) | None => break,
        }
    }
    for r in &loops {
        let _ = r.cmds.send(Cmd::Quit);
    }
    for r in loops {
        let _ = r.task.await;
    }
    crate::log::write("INFO", "daemon stopped");
    let _ = std::fs::remove_file(socket_path());
    Ok(())
}

/// Brings the running loops in line with the accounts in the config.
async fn reconcile(loops: &mut Vec<Running>, board: &Shared, tray: &Option<ksni::Handle<Tray>>, always_photos: bool) {
    let accounts = crate::account::all();
    // A loop is stopped, and waited for, before anything replaces it: two
    // loops on one account would both write its state.
    let mut i = 0;
    while i < loops.len() {
        let r = &loops[i];
        let wanted = accounts.iter().any(|a| *a == r.account && a.sync_folder() == r.folder);
        if wanted {
            i += 1;
            continue;
        }
        let r = loops.remove(i);
        let _ = r.cmds.send(Cmd::Quit);
        let _ = r.task.await;
        // A loop that was mid-pass when its account was removed saved its
        // state on the way out, after the removal cleared it.
        if !accounts.contains(&r.account) {
            let _ = std::fs::remove_dir_all(r.account.dir());
        }
    }
    for account in &accounts {
        if loops.iter().any(|r| r.account == *account) {
            continue;
        }
        let (cmds, rx) = mpsc::unbounded_channel();
        let folder = account.sync_folder();
        // Local: the sync's futures are not all Send, and nothing is gained by
        // moving an account's loop between threads.
        let task = tokio::task::spawn_local(account_loop(account.clone(), board.clone(), rx, tray.clone(), always_photos));
        loops.push(Running { account: account.clone(), folder, cmds, task });
    }
    {
        let mut b = board.write().expect("board lock");
        let mut old = std::mem::take(&mut b.accounts);
        for account in &accounts {
            let snap = match old.iter().position(|(u, _)| *u == account.username) {
                Some(at) => old.remove(at).1,
                None => Snapshot { root: account.sync_folder().unwrap_or_default(), ..Default::default() },
            };
            b.accounts.push((account.username.clone(), snap));
        }
    }
    if let Some(t) = tray {
        t.update(|_| {}).await;
    }
}

/// One account's loop, with every line it logs tagged with the account.
async fn account_loop(account: Account, board: Shared, rx: mpsc::UnboundedReceiver<Cmd>, tray: Option<ksni::Handle<Tray>>, always_photos: bool) {
    let name = account.username.clone();
    let result = crate::log::for_account(&name, sync_account(&account, &board, rx, &tray, always_photos)).await;
    if let Err(e) = result {
        crate::log::for_account(&name, async { crate::log::error(&format!("stopped syncing: {e:#}")) }).await;
        board.write().expect("board lock").update(&name, |s| s.last_error = Some(format!("{e:#}")));
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }
    }
}

async fn sync_account(
    account: &Account,
    board: &Shared,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    tray: &Option<ksni::Handle<Tray>>,
    always_photos: bool,
) -> Result<()> {
    let name = account.username.as_str();
    let update_tray = || async {
        if let Some(t) = tray {
            t.update(|_| {}).await;
        }
    };
    let mut state = sync::open_state(account)?;
    match account.sync_folder() {
        Some(folder) => state.root = folder,
        // Added by hand to the config, or by a version that set no folder.
        None => {
            if state.root.as_os_str().is_empty() {
                state.root = crate::config::default_sync_folder()?;
            }
            account.check_folder(&state.root)?;
            let root = state.root.clone();
            account.update(|a| a.sync_folder = Some(root))?;
        }
    }
    refresh(board, name, &state, false, None, Health::Ok);
    refresh_photos(board, account);

    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<()>();
    let watcher = watch(&state.root, fs_tx);
    if watcher.is_some() {
        crate::log::write("INFO", &format!("watching {}", state.root.display()));
    }
    let mut last_sweep = std::time::Instant::now();
    let mut fs_dirty = false;
    let mut force = false;
    // Consecutive failed passes: the wait grows and the user hears about the
    // outage once, not every 30 seconds.
    let mut failures: u32 = 0;
    // Set while the keyring has no session to work with. Passes stop until
    // one appears rather than failing every thirty seconds.
    let mut signed_out = false;
    let mut outage = Outage::default();
    // None until the first look, so switching photos on in the window starts
    // fetching them at the next pass rather than half an hour later.
    let mut last_photos: Option<std::time::Instant> = None;
    let mut ingest_seen = crate::ingest::Seen::default();
    // The last ingestion failure, so a standing one is logged once rather
    // than every thirty seconds.
    let mut ingest_error: Option<String> = None;
    // Opened at the first pass, and again whenever the session in hand dies:
    // the window can sign out and back in while this runs, and only the
    // keyring knows the new session.
    let mut drive = None;
    let mut session = None;
    let mut had_drive = false;
    loop {
        let mut health = Health::Ok;
        if drive.is_none() {
            match account.open_drive().await {
                Ok((fresh, s)) => {
                    drive = Some(fresh);
                    session = Some(s);
                    if signed_out {
                        crate::log::write("INFO", "signed in again; syncing resumed");
                        notify(crate::i18n::t("Signed in again. Syncing resumed."));
                    } else if had_drive {
                        crate::log::write("INFO", "the session changed; picked up the new one");
                    }
                    signed_out = false;
                    had_drive = true;
                }
                Err(e) => {
                    // A session revoked elsewhere is as good as none.
                    let no_session = crate::api::session_expired(&e) || matches!(account.session().await, Ok(None));
                    let error = if no_session {
                        // Signed out while running is worth a word; starting
                        // without a session is how a signed-out account is.
                        if !signed_out && had_drive {
                            crate::log::warn("signed out elsewhere; waiting for a sign-in");
                            notify(crate::i18n::t("Signed out. Sign in from the account window to resume syncing."));
                        }
                        signed_out = true;
                        health = Health::SignedOut;
                        None
                    } else if crate::api::offline(&e) {
                        let reason = crate::api::offline_reason(&e);
                        if let Some(line) = outage.failing(std::time::Instant::now(), &reason) {
                            crate::log::warn(&line);
                            notify(&line);
                        }
                        failures += 1;
                        health = Health::Offline;
                        Some(reason)
                    } else {
                        let msg = format!("{e:#}");
                        if failures == 0 {
                            crate::log::error(&format!("cannot open the account: {msg}"));
                        }
                        failures += 1;
                        Some(msg)
                    };
                    refresh(board, name, &state, false, error, health);
                    update_tray().await;
                    let wait = if signed_out { POLL } else { POLL * 2u32.pow(failures.min(3)) };
                    if !wait_for_work(&mut rx, &mut fs_rx, wait, &mut force, &mut fs_dirty).await? {
                        break;
                    }
                    continue;
                }
            }
        }
        let d = drive.as_mut().expect("opened above");
        // Paused: no pass at all, but keep waiting as usual, so a resume, a
        // network change or Quit is noticed. Local changes meanwhile stay
        // flagged and are swept once it resumes.
        let pause = crate::pause::reason();
        let noticed = {
            let mut b = board.write().expect("board lock");
            let changed = b.paused != pause;
            b.paused = pause.clone();
            changed
        };
        if noticed {
            // Whichever account's loop sees it first says so, once.
            match &pause {
                Some(why) => crate::log::write("INFO", why),
                None => crate::log::write("INFO", "sync resumed"),
            }
            update_tray().await;
        }
        if pause.is_some() {
            force = false;
            if !wait_for_work(&mut rx, &mut fs_rx, POLL, &mut force, &mut fs_dirty).await? {
                break;
            }
            continue;
        }
        // Sweep the folder only when something says to: a burst of events, no
        // watcher at all, the hourly safety net, or an explicit Sync now.
        let check_local = force || fs_dirty || watcher.is_none() || last_sweep.elapsed() >= SWEEP_EVERY;
        if check_local {
            last_sweep = std::time::Instant::now();
        }
        fs_dirty = false;
        refresh(board, name, &state, true, None, Health::Ok);
        update_tray().await;
        let error = match sync::run_with(d, &mut state, force, check_local).await {
            Ok(Some(notes)) => {
                crate::log::info(&format!("synced to {}", state.root.display()));
                if !notes.is_empty() {
                    notify(&notes.join("\n"));
                }
                None
            }
            Ok(None) => None,
            // A session that died under us is not a sync failure: the window
            // signed out, or signed in again and stored a different session.
            // The next turn takes whatever the keyring holds now.
            Err(e) if crate::api::session_expired(&e) => {
                drive = None;
                continue;
            }
            // Nothing reached Proton: a state to show, not a failure to
            // record every thirty seconds. The tray and the window carry it
            // for as long as it lasts; the log gets the two ends of it.
            Err(e) if crate::api::offline(&e) => {
                let reason = crate::api::offline_reason(&e);
                if let Some(line) = outage.failing(std::time::Instant::now(), &reason) {
                    crate::log::warn(&line);
                    notify(&line);
                }
                failures += 1;
                health = Health::Offline;
                Some(reason)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                crate::log::error(&format!("sync error: {msg}"));
                if failures == 0 {
                    notify(&msg);
                }
                failures += 1;
                Some(msg)
            }
        };
        if error.is_none() {
            match outage.ended(std::time::Instant::now()) {
                Some(line) => {
                    crate::log::warn(&line);
                    notify(&line);
                }
                None if failures > 0 => notify(crate::i18n::t("Sync resumed")),
                None => {}
            }
            failures = 0;
        }
        // Photos ride along with a pass that worked: the timeline is a second
        // library on a volume of its own, and download only.
        if error.is_none() && (always_photos || account.settings().photos_sync) {
            let due = last_photos.map(|t| t.elapsed() >= PHOTOS_EVERY).unwrap_or(true);
            if due {
                last_photos = Some(std::time::Instant::now());
                match crate::photos::pass(d, account).await {
                    Ok(None) => crate::log::write("INFO", "this account has no Proton Photos library"),
                    Ok(Some((0, _))) => {}
                    Ok(Some((n, dest))) => {
                        crate::log::write("INFO", &format!("{n} photo(s) into {}", dest.display()));
                        notify(&crate::i18n::fill(
                            crate::i18n::tn("{n} photo downloaded", "{n} photos downloaded", n as u64),
                            &[("n", &n.to_string())],
                        ));
                    }
                    // Photos are a side errand: a failure there says nothing
                    // about the folder, which has already synced.
                    Err(e) => crate::log::error(&format!("photos: {e:#}")),
                }
                refresh_photos(board, account);
            }
        }
        // Ingestion every pass, not half-hourly: a photo dropped in should go
        // up within a minute, and listing one folder is cheap.
        if error.is_none() && account.ingest_folder().is_some() {
            match crate::ingest::pass(d, account, &mut ingest_seen).await {
                Ok(n) => {
                    ingest_error = None;
                    if n > 0 {
                        notify(&crate::i18n::fill(
                            crate::i18n::tn("{n} photo uploaded to Proton Photos", "{n} photos uploaded to Proton Photos", n as u64),
                            &[("n", &n.to_string())],
                        ));
                    }
                }
                Err(e) => {
                    let e = format!("photo ingestion: {e:#}");
                    if ingest_error.as_ref() != Some(&e) {
                        crate::log::error(&e);
                        ingest_error = Some(e);
                    }
                }
            }
        }
        force = false;
        // Rotated tokens reach the keyring, or the next start would find a
        // refresh token that no longer works.
        if let Some(s) = d.api.session().filter(|s| session.as_ref() != Some(s)) {
            if let Err(e) = account.save_session(&s).await {
                crate::log::error(&format!("could not store the refreshed session: {e:#}"));
            }
            session = Some(s);
        }
        refresh(board, name, &state, false, error, health);
        // A pass happened, whatever it found: that is what "checked" means.
        board.write().expect("board lock").update(name, |s| s.last_sync = Some(now()));
        update_tray().await;

        // A fixed 30 s poll of the event stream while healthy (nothing pushes
        // from Proton), doubling per failed pass up to 16 minutes. An outage
        // stops sooner at four: asking whether the network is back costs a
        // failed DNS lookup, and waiting a quarter of an hour to find out it
        // returned is worse than the lookup.
        let ceiling = match health {
            Health::Offline => 3,
            _ => 5,
        };
        let wait = POLL * 2u32.pow(failures.min(ceiling));
        if !wait_for_work(&mut rx, &mut fs_rx, wait, &mut force, &mut fs_dirty).await? {
            break;
        }
    }
    drop(watcher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zombies_are_not_running() {
        assert!(zombie("188755 (kpdrive-ui) Z 188605 188755"));
        assert!(!zombie("188755 (kpdrive-ui) S 188605 188755"));
        assert!(!zombie("42 (odd) Z name) R 1 42"));
        assert!(zombie("42 (odd) S name) Z 1 42"));
    }

    #[test]
    fn detached_children_are_reaped() {
        let pid = spawn_detached(&mut std::process::Command::new("true")).unwrap();
        let stat = format!("/proc/{pid}/stat");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_to_string(&stat).is_ok_and(|s| s.split_whitespace().nth(3) == Some(std::process::id().to_string()).as_deref()) {
            assert!(std::time::Instant::now() < deadline, "child {pid} was never reaped");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn an_outage_is_two_lines_however_long_it_lasts() {
        let t0 = std::time::Instant::now();
        let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
        let mut o = Outage::default();

        // One line when it goes, nothing for the passes that keep failing.
        assert!(o.failing(at(0), "cannot reach Proton Drive: no route").is_some());
        assert_eq!(o.failing(at(30), "cannot reach Proton Drive: no route"), None);
        assert_eq!(o.failing(at(900), "cannot reach Proton Drive: no route"), None);
        // One when it comes back, saying how long it was gone.
        let back = o.ended(at(960)).expect("the all-clear");
        assert!(back.contains("16 minutes"), "{back}");
        assert_eq!(o.ended(at(990)), None, "a pass that succeeds while online says nothing");

        // A link that dips straight after is the same episode: no second pair.
        assert_eq!(o.failing(at(1000), "cannot reach Proton Drive: no route"), None);
        assert_eq!(o.ended(at(1010)), None);
        assert_eq!(o.failing(at(1020), "cannot reach Proton Drive: no route"), None);

        // Unless it stays down: then it is a real outage and gets its line,
        // and its own all-clear.
        assert!(o.failing(at(1020 + 301), "cannot reach Proton Drive: no route").is_some());
        assert!(o.ended(at(1020 + 400)).is_some());

        // Long after the last report, a fresh outage reports at once again.
        let mut o = Outage::default();
        assert!(o.failing(at(0), "x").is_some());
        assert!(o.ended(at(60)).is_some());
        assert!(o.failing(at(60 + 301), "x").is_some(), "quiet for long enough is a new episode");
    }

    #[test]
    fn the_sentence_says_what_matters_most_first() {
        let alice = AccountReport { username: "alice".into(), items: 3, last_sync: Some(now()), ..Default::default() };
        let running = Report { running: true, answered: true, accounts: vec![alice.clone()], ..Default::default() };
        let say = |r: &Report| r.sentence(Some("alice"));
        let mute = Report { running: true, ..Default::default() };
        assert!(say(&mute).contains("too old"), "{}", say(&mute));
        assert!(say(&Report::default()).contains("not running"));
        assert!(say(&running).contains("3 items in sync"), "{}", say(&running));
        assert!(say(&running).contains("just now"));
        assert_eq!(running.sentence(None), say(&running), "no name means the first account");

        let with = |a: AccountReport| Report { accounts: vec![a], ..running.clone() };
        let stale = with(AccountReport { last_sync: Some(now() - 600), ..alice.clone() });
        assert!(say(&stale).contains("10 minutes ago"), "{}", say(&stale));

        let failed = AccountReport { error: Some("boom".into()), ..alice.clone() };
        assert!(say(&with(failed.clone())).contains("boom"));

        let syncing = with(AccountReport { syncing: true, ..failed.clone() });
        assert!(say(&syncing).contains("boom"), "a failure outranks being busy");

        let off = with(AccountReport { offline: true, error: Some("cannot reach Proton Drive: no route".into()), ..alice.clone() });
        assert!(say(&off).contains("Still trying"), "an outage is not a failed sync: {}", say(&off));
        assert!(!say(&off).contains("Last sync failed"));

        let out = Report { accounts: vec![AccountReport { signed_out: true, ..failed.clone() }], ..running.clone() };
        assert!(say(&out).contains("Signed out"), "being signed out outranks the error it caused");

        let paused = Report { paused: Some("Paused".into()), ..out.clone() };
        assert_eq!(say(&paused), "Paused", "a pause outranks everything the accounts say");

        let gone = Report { running: false, ..out.clone() };
        assert!(say(&gone).contains("not running"), "nothing else matters if it is not there");

        let unknown = running.sentence(Some("bob"));
        assert!(unknown.contains("Signed out"), "an account the daemon has not started yet: {unknown}");
        let old = Report { accounts: vec![AccountReport { username: String::new(), ..alice.clone() }], ..running.clone() };
        assert!(old.sentence(Some("bob")).contains("3 items"), "a daemon older than accounts answers for anyone");
    }

    #[test]
    fn the_board_speaks_for_every_account() {
        let mut board = Board::default();
        assert!(board.report().contains("\"signedOut\":true"), "no account is signed out");
        let snap = |root: &str, items: usize| Snapshot {
            root: root.into(),
            entries: (0..items).map(|i| (PathBuf::from(i.to_string()), Entry { path: i.to_string().into(), is_folder: true, revision: None, mtime: 0, size: 0 })).collect(),
            ..Default::default()
        };
        board.accounts.push(("alice".into(), snap("/a", 2)));
        board.accounts.push(("bob".into(), snap("/b", 3)));
        board.update("carol", |s| s.syncing = true);
        assert_eq!(board.accounts.len(), 2, "an update never adds an account");
        board.update("bob", |s| s.last_error = Some("boom".into()));
        let v: serde_json::Value = serde_json::from_str(&board.report()).unwrap();
        assert_eq!(v["items"], 5);
        assert_eq!(v["root"], "/a");
        assert_eq!(v["error"], "boom");
        assert_eq!(v["accounts"][1]["username"], "bob");
        assert_eq!(v["accounts"][1]["error"], "boom");
        let report = Report { running: true, answered: true, accounts: v["accounts"].as_array().unwrap().iter().map(AccountReport::parse).collect(), ..Default::default() };
        assert!(report.sentence(Some("BOB")).contains("boom"));
        assert!(report.sentence(Some("alice")).contains("2 items"));
        assert_eq!(board.status(Path::new("/b/1")), "OK");
        assert_eq!(board.status(Path::new("/a/0")), "OK");
        assert_eq!(board.status(Path::new("/c")), "NONE");
        assert_eq!(serde_json::from_str::<Vec<String>>(&board.folders()).unwrap(), ["/a", "/b"]);
    }

    #[test]
    fn spots_a_process_that_is_already_running() {
        let me = std::env::current_exe().expect("this test is a process");
        assert!(running(&me), "the test binary is running, being the one asking");
        assert!(!running(Path::new("/usr/bin/kpdrive-no-such-window")));
    }

    #[test]
    fn status_classification() {
        let dir = std::env::temp_dir().join(format!("kpdrive-daemon-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a"), b"abc").unwrap();
        let m = std::fs::metadata(dir.join("a")).unwrap();
        let mtime = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let mut snap = Snapshot { root: dir.clone(), ..Default::default() };
        snap.entries.insert("a".into(), Entry { path: "a".into(), is_folder: false, revision: None, mtime, size: 3 });
        snap.entries.insert("sub".into(), Entry { path: "sub".into(), is_folder: true, revision: None, mtime: 0, size: 0 });
        assert_eq!(status(&snap, &dir.join("a")), "OK");
        assert_eq!(status(&snap, &dir.join("sub")), "OK");
        assert_eq!(status(&snap, &dir), "OK");
        assert_eq!(status(&snap, &dir.join("new")), "SYNC");
        assert_eq!(status(&snap, Path::new("/etc/passwd")), "NONE");
        std::fs::write(dir.join("a"), b"abcd").unwrap();
        assert_eq!(status(&snap, &dir.join("a")), "SYNC");
        std::fs::remove_dir_all(&dir).unwrap();

        snap.photos_root = Some("/pics".into());
        snap.photos = Path::new("2024/03/a.jpg").ancestors().map(Path::to_path_buf).collect();
        for ok in ["/pics", "/pics/2024", "/pics/2024/03", "/pics/2024/03/a.jpg"] {
            assert_eq!(status(&snap, Path::new(ok)), "OK", "{ok}");
        }
        assert_eq!(status(&snap, Path::new("/pics/2024/03/mine.jpg")), "NONE");
        assert_eq!(status(&snap, Path::new("/pictures")), "NONE");
    }
}

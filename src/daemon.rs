//! Long-running mode: periodic sync, Plasma tray icon, and a Unix socket that
//! the Dolphin overlay plugin (and anyone else) can ask for per-file status.

use anyhow::{Context, Result};
use proton_crypto::crypto::PGPProviderSync;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::drive::Drive;
use crate::sync::{self, Entry, State, local_matches};

const POLL: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a burst of filesystem events must be quiet before a pass starts:
/// an editor's write-temp-then-rename, or a lock file, then counts as one edit.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);
/// A burst that never goes quiet still gets a pass this often.
const DEBOUNCE_CAP: std::time::Duration = std::time::Duration::from_secs(30);
/// Even with inotify, sweep the folder now and then: network mounts and watch
/// limits can lose events.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

/// What the tray, the socket and the window see; refreshed after every pass.
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
}

impl Snapshot {
    /// One line of JSON for the window, which has no other way to know what
    /// this daemon is doing.
    fn report(&self) -> String {
        serde_json::json!({
            "root": self.root.display().to_string(),
            "items": self.entries.len(),
            "syncing": self.syncing,
            "signedOut": self.signed_out,
            "offline": self.offline,
            "lastSync": self.last_sync,
            "error": self.last_error,
        })
        .to_string()
    }
}

type Shared = Arc<RwLock<Snapshot>>;

enum Cmd {
    Sync,
    Quit,
}

struct Tray {
    snap: Shared,
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
        let s = self.snap.read().expect("snapshot lock");
        if s.last_error.is_some() || s.signed_out { "emblem-error".into() } else { String::new() }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let s = self.snap.read().expect("snapshot lock");
        let description = match (s.signed_out, &s.last_error, s.syncing) {
            (true, ..) => "Signed out. Sign in from the account window.".into(),
            (_, Some(e), _) => e.clone(),
            (_, None, true) => "Syncing…".into(),
            (_, None, false) => format!("{} items in sync", s.entries.len()),
        };
        ksni::ToolTip { title: "Proton Drive".into(), description, ..Default::default() }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Open folder".into(),
                icon_name: "folder-open".into(),
                activate: Box::new(|t: &mut Self| {
                    let root = t.snap.read().expect("snapshot lock").root.clone();
                    let _ = std::process::Command::new("xdg-open").arg(root).spawn();
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Account and logs…".into(),
                icon_name: "user-identity".into(),
                activate: Box::new(|_: &mut Self| open_window()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Sync now".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmds.send(Cmd::Sync);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
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
    if let Err(e) = std::process::Command::new(&exe).spawn() {
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
    })
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("kpdrive.sock")
}

/// Per-path status for overlays: OK, SYNC (pending), NONE (not ours).
fn status(snap: &Snapshot, path: &Path) -> &'static str {
    let Ok(rel) = path.strip_prefix(&snap.root) else { return "NONE" };
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

async fn serve_socket(snap: Shared, cmds: mpsc::UnboundedSender<Cmd>) -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let snap = snap.clone();
        let cmds = cmds.clone();
        tokio::spawn(async move {
            let (r, mut w) = stream.into_split();
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply = match line.split_once(' ') {
                    Some(("STATUS", p)) => status(&snap.read().expect("snapshot lock"), Path::new(p)).to_string(),
                    _ if line == "ROOT" => snap.read().expect("snapshot lock").root.display().to_string(),
                    _ if line == "STATE" => snap.read().expect("snapshot lock").report(),
                    _ if line == "SYNC" => {
                        let _ = cmds.send(Cmd::Sync);
                        "OK".into()
                    }
                    _ if line == "QUIT" => {
                        let _ = cmds.send(Cmd::Quit);
                        "OK".into()
                    }
                    _ => "ERR".into(),
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
        Some(format!("{reason}; retrying"))
    }

    /// A pass succeeded. Gives the all-clear, if the outage was mentioned.
    fn ended(&mut self, now: std::time::Instant) -> Option<String> {
        let since = self.since.take()?;
        if !std::mem::take(&mut self.reported) {
            return None;
        }
        self.last_report = Some(now);
        Some(format!("Proton Drive is reachable again, after {}", spell(now.duration_since(since))))
    }
}

/// A rough duration, in the largest unit that still says something.
fn spell(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=90 => format!("{secs} seconds"),
        91..=5400 => format!("{} minutes", secs / 60),
        _ => format!("{} hours", secs / 3600),
    }
}

/// What a daemon is doing, as anything outside it sees it. `running: false`
/// is the answer when there is no daemon to ask.
#[derive(Default, Debug, Clone)]
pub struct Report {
    pub running: bool,
    /// Whether it answered the question. An older daemon holds the socket but
    /// does not know this command.
    pub answered: bool,
    pub syncing: bool,
    pub signed_out: bool,
    /// Proton cannot be reached. Still trying, and nothing is wrong as such.
    pub offline: bool,
    pub items: usize,
    pub last_sync: Option<i64>,
    pub error: Option<String>,
}

impl Report {
    /// One sentence, worded once, so the window and `kpdrive status` say the
    /// same thing about the same daemon.
    pub fn sentence(&self) -> String {
        if !self.running {
            return "The sync daemon is not running. Start it with: kpdrive sync --watch".into();
        }
        if !self.answered {
            return "A sync daemon is running but is too old to say what it is doing. Restart it.".into();
        }
        if self.signed_out {
            return "Signed out. Syncing resumes once you sign in.".into();
        }
        if let Some(e) = &self.error {
            return match self.offline {
                true => format!("{e}. Still trying."),
                false => format!("Last sync failed: {e}"),
            };
        }
        if self.syncing {
            return "Syncing…".into();
        }
        let items = format!("{} item{} in sync", self.items, if self.items == 1 { "" } else { "s" });
        match self.last_sync.map(|t| now() - t) {
            Some(secs) if secs < 90 => format!("{items}, checked just now"),
            Some(secs) if secs < 5400 => format!("{items}, checked {} minutes ago", secs / 60),
            Some(secs) => format!("{items}, checked {} hours ago", secs / 3600),
            None => items,
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
        report.syncing = v["syncing"].as_bool().unwrap_or(false);
        report.signed_out = v["signedOut"].as_bool().unwrap_or(false);
        report.offline = v["offline"].as_bool().unwrap_or(false);
        report.items = v["items"].as_u64().unwrap_or(0) as usize;
        report.last_sync = v["lastSync"].as_i64();
        report.error = v["error"].as_str().map(str::to_owned);
    }
    report
}

/// Tells a running daemon to sync now, which is also how it is nudged to pick
/// up a session that has just changed. Silent when there is no daemon.
pub fn poke() {
    use std::io::Write;
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket_path()) {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(300)));
        let _ = stream.write_all(b"SYNC\n");
    }
}

pub fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "kpdrive", "-i", "folder-cloud", "Proton Drive", body])
        .spawn();
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

fn refresh(snap: &Shared, state: &State, syncing: bool, error: Option<String>, health: Health) {
    let mut s = snap.write().expect("snapshot lock");
    s.root = state.root.clone();
    s.entries = state.nodes.values().map(|e| (e.path.clone(), e.clone())).collect();
    s.syncing = syncing;
    s.last_error = error;
    s.signed_out = health == Health::SignedOut;
    s.offline = health == Health::Offline;
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
            Some(Cmd::Sync) => {
                *force = true;
                // Several clicks during one pass mean one forced pass, not several.
                while let Ok(Cmd::Sync) = rx.try_recv() {}
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

/// Runs until Quit.
///
/// `persist` is called with the session after each pass so rotated tokens
/// reach the keyring. `reopen` builds a fresh [`Drive`] from whatever session
/// the keyring holds now: the window can sign out and back in while this is
/// running, which leaves the session in hand dead, and only the keyring knows
/// the new one.
pub async fn run<P, F, Fut>(
    mut drive: Drive<P>,
    mut state: State,
    mut persist: impl FnMut(&Drive<P>),
    reopen: F,
) -> Result<()>
where
    P: PGPProviderSync,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Drive<P>>>,
{
    let snap: Shared = Arc::default();
    refresh(&snap, &state, false, None, Health::Ok);
    let (tx, mut rx) = mpsc::unbounded_channel();

    {
        let (snap, tx) = (snap.clone(), tx.clone());
        tokio::spawn(async move {
            if let Err(e) = serve_socket(snap, tx).await {
                // Without it the overlay icons and the window learn nothing.
                crate::log::error(&format!("status socket unavailable: {e:#}"));
            }
        });
    }
    let tray = match ksni::TrayMethods::spawn(Tray { snap: snap.clone(), cmds: tx.clone() }).await {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("tray unavailable: {e}");
            None
        }
    };

    // Opens this run in the log; the window shows everything after it.
    crate::log::mark("daemon started");
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
    // Set when the keyring has no session to work with. Passes stop until one
    // appears rather than failing every thirty seconds.
    let mut signed_out = false;
    let mut outage = Outage::default();
    loop {
        let mut retry_now = false;
        let mut health = Health::Ok;
        if signed_out {
            match reopen().await {
                Ok(fresh) => {
                    drive = fresh;
                    signed_out = false;
                    failures = 0;
                    crate::log::write("INFO", "signed in again; syncing resumed");
                    notify("Signed in again. Syncing resumed.");
                }
                Err(_) => {
                    // Still nothing. Wait for a sign-in, quietly, but stay as
                    // ready to quit as any other wait is.
                    refresh(&snap, &state, false, None, Health::SignedOut);
                    if let Some(t) = &tray {
                        t.update(|_| {}).await;
                    }
                    if !wait_for_work(&mut rx, &mut fs_rx, POLL, &mut force, &mut fs_dirty).await? {
                        break;
                    }
                    continue;
                }
            }
        }
        // Sweep the folder only when something says to: a burst of events, no
        // watcher at all, the hourly safety net, or an explicit Sync now.
        let check_local = force || fs_dirty || watcher.is_none() || last_sweep.elapsed() >= SWEEP_EVERY;
        if check_local {
            last_sweep = std::time::Instant::now();
        }
        fs_dirty = false;
        // Cheap: a directory listing, once per pass.
        if let Err(e) = crate::log::prune(crate::config::load().log_retention_days) {
            crate::log::error(&format!("log retention: {e:#}"));
        }
        refresh(&snap, &state, true, None, Health::Ok);
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }
        let error = match sync::run_with(&mut drive, &mut state, force, check_local).await {
            Ok(Some(notes)) => {
                crate::log::write("INFO", &format!("synced to {}", state.root.display()));
                println!("synced to {}", state.root.display());
                if !notes.is_empty() {
                    notify(&notes.join("\n"));
                }
                None
            }
            Ok(None) => None,
            // A session that died under us is not a sync failure: the window
            // signed out, or signed in again and stored a different session.
            // Take whatever the keyring holds now and carry on.
            Err(e) if crate::api::session_expired(&e) => match reopen().await {
                Ok(fresh) => {
                    drive = fresh;
                    retry_now = true;
                    crate::log::write("INFO", "the session changed; picked up the new one");
                    None
                }
                Err(_) => {
                    signed_out = true;
                    crate::log::warn("signed out elsewhere; waiting for a sign-in");
                    notify("Signed out. Sign in from the account window to resume syncing.");
                    continue;
                }
            },
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
                None if failures > 0 => notify("Sync resumed"),
                None => {}
            }
            failures = 0;
        }
        force = false;
        persist(&drive);
        refresh(&snap, &state, false, error, health);
        // A pass happened, whatever it found: that is what "checked" means.
        snap.write().expect("snapshot lock").last_sync = Some(now());
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }
        // A pass that ended only to take on a new session runs again at once.
        if retry_now {
            force = true;
            continue;
        }

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
    crate::log::write("INFO", "daemon stopped");
    let _ = std::fs::remove_file(socket_path());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let running = Report { running: true, answered: true, items: 3, last_sync: Some(now()), ..Default::default() };
        let mute = Report { running: true, ..Default::default() };
        assert!(mute.sentence().contains("too old"), "{}", mute.sentence());
        assert!(Report::default().sentence().contains("not running"));
        assert!(running.sentence().contains("3 items in sync"), "{}", running.sentence());
        assert!(running.sentence().contains("just now"));

        let stale = Report { last_sync: Some(now() - 600), ..running.clone() };
        assert!(stale.sentence().contains("10 minutes ago"), "{}", stale.sentence());

        let failed = Report { error: Some("boom".into()), ..running.clone() };
        assert!(failed.sentence().contains("boom"));

        let syncing = Report { syncing: true, ..failed.clone() };
        assert!(syncing.sentence().contains("boom"), "a failure outranks being busy");

        let off = Report { offline: true, error: Some("cannot reach Proton Drive: no route".into()), ..running.clone() };
        assert!(off.sentence().contains("Still trying"), "an outage is not a failed sync: {}", off.sentence());
        assert!(!off.sentence().contains("Last sync failed"));

        let out = Report { signed_out: true, ..failed.clone() };
        assert!(out.sentence().contains("Signed out"), "being signed out outranks the error it caused");

        let gone = Report { running: false, ..out.clone() };
        assert!(gone.sentence().contains("not running"), "nothing else matters if it is not there");
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
    }
}

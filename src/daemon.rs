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

/// What the tray and the socket see; refreshed after every pass.
#[derive(Default)]
struct Snapshot {
    root: PathBuf,
    entries: HashMap<PathBuf, Entry>,
    syncing: bool,
    last_error: Option<String>,
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
    fn title(&self) -> String {
        "Proton Drive".into()
    }
    fn icon_name(&self) -> String {
        "folder-cloud".into()
    }
    fn overlay_icon_name(&self) -> String {
        let s = self.snap.read().expect("snapshot lock");
        if s.last_error.is_some() { "emblem-error".into() } else { String::new() }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let s = self.snap.read().expect("snapshot lock");
        let description = match (&s.last_error, s.syncing) {
            (Some(e), _) => e.clone(),
            (None, true) => "Syncing…".into(),
            (None, false) => format!("{} items in sync", s.entries.len()),
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
                activate: Box::new(|_: &mut Self| {
                    match crate::setup::ui_binary() {
                        Ok(exe) => {
                            if let Err(e) = std::process::Command::new(&exe).spawn() {
                                crate::log::error(&format!("cannot start {}: {e}", exe.display()));
                            }
                        }
                        Err(e) => crate::log::error(&format!("cannot locate the window: {e:#}")),
                    }
                }),
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

pub fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "kpdrive", "-i", "folder-cloud", "Proton Drive", body])
        .spawn();
}

fn refresh(snap: &Shared, state: &State, syncing: bool, error: Option<String>) {
    let mut s = snap.write().expect("snapshot lock");
    s.root = state.root.clone();
    s.entries = state.nodes.values().map(|e| (e.path.clone(), e.clone())).collect();
    s.syncing = syncing;
    s.last_error = error;
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

/// Runs until Quit. `persist` is called with the session after each pass so
/// rotated tokens reach the wallet.
pub async fn run<P: PGPProviderSync>(
    mut drive: Drive<P>,
    mut state: State,
    mut persist: impl FnMut(&Drive<P>),
) -> Result<()> {
    let snap: Shared = Arc::default();
    refresh(&snap, &state, false, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    tokio::spawn(serve_socket(snap.clone(), tx.clone()));
    let tray = match ksni::TrayMethods::spawn(Tray { snap: snap.clone(), cmds: tx.clone() }).await {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("tray unavailable: {e}");
            None
        }
    };

    crate::log::write("INFO", "daemon started");
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
    loop {
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
        refresh(&snap, &state, true, None);
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
            Err(e) => {
                let msg = format!("sync error: {e:#}");
                crate::log::error(&msg);
                if failures == 0 {
                    notify(&msg);
                }
                failures += 1;
                Some(msg)
            }
        };
        if error.is_none() && failures > 0 {
            notify("Sync resumed");
            failures = 0;
        }
        force = false;
        persist(&drive);
        refresh(&snap, &state, false, error);
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }

        // A fixed 30 s poll of the event stream while healthy (nothing pushes
        // from Proton), doubling per failed pass up to 16 minutes.
        let wait = POLL * 2u32.pow(failures.min(5));
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::Sync) => {
                    force = true;
                    // Several clicks during one pass mean one forced pass, not several.
                    while let Ok(Cmd::Sync) = rx.try_recv() {}
                }
                Some(Cmd::Quit) | None => break,
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
                fs_dirty = true;
            }
            _ = tokio::time::sleep(wait) => {}
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

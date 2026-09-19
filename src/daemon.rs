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

    let mut force = false;
    loop {
        refresh(&snap, &state, true, None);
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }
        let error = match sync::run(&mut drive, &mut state, force).await {
            Ok(Some(notes)) => {
                println!("synced to {}", state.root.display());
                if !notes.is_empty() {
                    notify(&notes.join("\n"));
                }
                None
            }
            Ok(None) => None,
            Err(e) => {
                let msg = format!("sync error: {e:#}");
                eprintln!("{msg}");
                notify(&msg);
                Some(msg)
            }
        };
        force = false;
        persist(&drive);
        refresh(&snap, &state, false, error);
        if let Some(t) = &tray {
            t.update(|_| {}).await;
        }

        // ponytail: fixed 30s poll of the event stream; long-poll/push if Proton ever offers it.
        match tokio::time::timeout(POLL, rx.recv()).await {
            Ok(Some(Cmd::Sync)) => force = true,
            Ok(Some(Cmd::Quit)) | Ok(None) => break,
            Err(_) => {}
        }
    }
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

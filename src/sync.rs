//! Two-way sync between Drive and a local folder.
//!
//! Proton asks clients to sync from events rather than poll the tree, so a
//! pass only walks the remote tree when the event stream reports a change or
//! the local folder differs from what we last wrote.
//! ponytail: full walk on any change; enumerate events and touch only the
//! affected nodes when the tree gets big enough for the walk to hurt.
//!
//! Rules, in order of who wins:
//! - A file edited locally is never overwritten; if the remote also changed it
//!   is reported and left alone (neither side is pushed).
//! - A file deleted locally that is unchanged remotely is trashed remotely.
//! - A file removed remotely is deleted locally only if untouched since we wrote it.

use anyhow::{Context, Result};
use proton_crypto::crypto::PGPProviderSync;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crate::drive::{Drive, Node};

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub root: PathBuf,
    pub event_id: Option<String>,
    /// link id → what we last wrote locally for it.
    pub nodes: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Entry {
    /// Relative to `State::root`.
    pub path: PathBuf,
    pub is_folder: bool,
    pub revision: Option<String>,
    /// mtime (unix secs) and size we set/observed after writing; used to detect local edits.
    pub mtime: i64,
    pub size: u64,
}

const PART_SUFFIX: &str = ".kpdrive-part";

pub fn state_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&std::env::var_os("HOME").expect("HOME")).join(".local/share"));
    base.join("kpdrive/state.json")
}

pub fn load_state() -> Result<Option<State>> {
    match fs::read(state_path()) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).context("state.json is corrupt")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("read state.json"),
    }
}

pub fn save_state(state: &State) -> Result<()> {
    let path = state_path();
    fs::create_dir_all(path.parent().expect("state path has a parent"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// One sync pass. `None` when nothing needed doing; otherwise the notable
/// lines (conflicts, errors) of the pass that ran.
pub async fn run<P: PGPProviderSync>(drive: &mut Drive<P>, state: &mut State, force: bool) -> Result<Option<Vec<String>>> {
    let mut notes: Vec<String> = Vec::new();
    macro_rules! note {
        ($($arg:tt)*) => {{ let s = format!($($arg)*); eprintln!("{s}"); notes.push(s); }};
    }
    fs::create_dir_all(&state.root).with_context(|| format!("create {}", state.root.display()))?;
    let (cursor, remote_changed) = drive.events_since(state.event_id.as_deref()).await?;
    if !force && !remote_changed && !local_changed(state) {
        state.event_id = Some(cursor);
        save_state(state)?;
        return Ok(None);
    }

    let mut seen: BTreeMap<String, Entry> = BTreeMap::new();
    let mut nodes: HashMap<String, Node<P::PrivateKey>> = HashMap::new();
    let mut to_trash: Vec<String> = Vec::new();

    // ---- remote → local ---------------------------------------------------
    let root = drive.root()?;
    let root_id = root.id.clone();
    nodes.insert(root_id.clone(), root);
    let mut stack = vec![(root_id.clone(), PathBuf::new())];
    while let Some((folder_id, rel)) = stack.pop() {
        let children = drive.list(&nodes[&folder_id]).await?;
        for node in children {
            let Some(name) = safe_name(&node.name) else {
                eprintln!("skip: unsafe name {:?}", node.name);
                continue;
            };
            let node_rel = rel.join(name);
            let local = state.root.join(&node_rel);
            let old = state.nodes.get(&node.id);

            // Same content under a new name or parent: rename instead of re-fetching.
            if let Some(old) = old {
                let old_local = state.root.join(&old.path);
                if old.path != node_rel && old.revision == node.revision && old_local.exists() && !local.exists() {
                    fs::rename(&old_local, &local).with_context(|| format!("rename {}", old_local.display()))?;
                    println!("moved {} -> {}", old.path.display(), node_rel.display());
                }
            }

            // We wrote it before, it is gone locally, and the remote is unchanged: the user deleted it.
            if let Some(old) = old {
                if !local.exists() && old.revision == node.revision {
                    println!("trash {} (deleted locally)", node_rel.display());
                    to_trash.push(node.id.clone());
                    continue;
                }
            }

            let entry = if node.is_folder {
                fs::create_dir_all(&local)?;
                stack.push((node.id.clone(), node_rel.clone()));
                Entry { path: node_rel, is_folder: true, revision: None, mtime: 0, size: 0 }
            } else {
                match sync_file(drive, &node, &local, old, &mut notes).await {
                    Ok(Some((mtime, size))) => Entry { path: node_rel, is_folder: false, revision: node.revision.clone(), mtime, size },
                    Ok(None) => {
                        nodes.insert(node.id.clone(), node);
                        continue; // conflict: kept local, not tracked this round
                    }
                    Err(e) => {
                        note!("error: {}: {e:#}", node_rel.display());
                        continue;
                    }
                }
            };
            seen.insert(node.id.clone(), entry);
            nodes.insert(node.id.clone(), node);
        }
    }

    // ---- local → remote ---------------------------------------------------
    let mut by_path: HashMap<PathBuf, String> = seen.iter().map(|(id, e)| (e.path.clone(), id.clone())).collect();
    by_path.insert(PathBuf::new(), root_id.clone());
    let old_paths: HashMap<&PathBuf, &String> = state.nodes.iter().map(|(id, e)| (&e.path, id)).collect();

    let mut queue = VecDeque::from([PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        let mut items: Vec<_> = fs::read_dir(state.root.join(&rel))?.filter_map(|e| e.ok()).collect();
        items.sort_by_key(|e| e.file_name());
        for item in items {
            let name = item.file_name();
            let Some(name) = name.to_str().and_then(safe_name) else { continue };
            if name.ends_with(PART_SUFFIX) {
                continue;
            }
            let item_rel = rel.join(name);
            let meta = item.metadata()?;
            if meta.is_dir() {
                if !by_path.contains_key(&item_rel) {
                    let parent_id = by_path[&rel].clone();
                    match drive.create_folder(&nodes[&parent_id], name).await {
                        Ok(node) => {
                            println!("pushed {}/", item_rel.display());
                            seen.insert(node.id.clone(), Entry { path: item_rel.clone(), is_folder: true, revision: None, mtime: 0, size: 0 });
                            by_path.insert(item_rel.clone(), node.id.clone());
                            nodes.insert(node.id.clone(), node);
                        }
                        Err(e) => {
                            note!("error: push {}/: {e:#}", item_rel.display());
                            continue;
                        }
                    }
                }
                queue.push_back(item_rel);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let mtime = mtime_of(&meta);
            let (existing, changed) = match by_path.get(&item_rel) {
                Some(id) => (Some(id.clone()), !local_matches(&meta, &seen[id])),
                None if old_paths.contains_key(&item_rel) => continue, // conflict reported above
                None => (None, true),
            };
            if !changed {
                continue;
            }
            let parent_id = by_path[&rel].clone();
            let mut file = match fs::File::open(item.path()) {
                Ok(f) => f,
                Err(e) => {
                    note!("error: open {}: {e}", item_rel.display());
                    continue;
                }
            };
            let existing_node = existing.as_ref().map(|id| &nodes[id]);
            match drive.upload(&nodes[&parent_id], name, existing_node, &mut file, mtime).await {
                Ok((id, revision)) => {
                    println!("pushed {} ({} bytes)", item_rel.display(), meta.len());
                    seen.insert(id.clone(), Entry { path: item_rel.clone(), is_folder: false, revision: Some(revision), mtime, size: meta.len() });
                    by_path.insert(item_rel, id);
                }
                Err(e) => note!("error: push {}: {e:#}", item_rel.display()),
            }
        }
    }

    if let Err(e) = drive.trash(&to_trash).await {
        note!("error: trash: {e:#}");
    }

    // ---- gone remotely: remove locally, but only what we wrote and the user hasn't touched.
    for (id, old) in &state.nodes {
        if seen.contains_key(id) || to_trash.contains(id) {
            continue;
        }
        let local = state.root.join(&old.path);
        let Ok(meta) = fs::metadata(&local) else { continue };
        if old.is_folder {
            if fs::read_dir(&local).map(|mut d| d.next().is_none()).unwrap_or(false) {
                fs::remove_dir(&local)?;
                println!("removed {}/", old.path.display());
            } else {
                note!("keep: {} removed remotely but not empty locally", old.path.display());
            }
        } else if local_matches(&meta, old) {
            fs::remove_file(&local)?;
            println!("removed {}", old.path.display());
        } else {
            note!("keep: {} removed remotely but edited locally", old.path.display());
        }
    }

    state.nodes = seen;
    state.event_id = Some(cursor);
    save_state(state)?;
    Ok(Some(notes))
}

/// Cheap local scan: anything new, edited or deleted since the last pass?
fn local_changed(state: &State) -> bool {
    let by_path: HashMap<&PathBuf, &Entry> = state.nodes.values().map(|e| (&e.path, e)).collect();
    for entry in state.nodes.values() {
        if !state.root.join(&entry.path).exists() {
            return true;
        }
    }
    let mut queue = VecDeque::from([PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        let Ok(dir) = fs::read_dir(state.root.join(&rel)) else { continue };
        for item in dir.filter_map(|e| e.ok()) {
            let name = item.file_name();
            let Some(name) = name.to_str().and_then(safe_name) else { continue };
            if name.ends_with(PART_SUFFIX) {
                continue;
            }
            let item_rel = rel.join(name);
            let Ok(meta) = item.metadata() else { continue };
            match by_path.get(&item_rel) {
                None => return true,
                Some(e) if meta.is_file() && !local_matches(&meta, e) => return true,
                _ => {}
            }
            if meta.is_dir() {
                queue.push_back(item_rel);
            }
        }
    }
    false
}

/// Downloads `node` to `local` when needed. Returns the (mtime, size) recorded,
/// or `None` when a locally edited file was left alone.
async fn sync_file<P: PGPProviderSync>(
    drive: &mut Drive<P>,
    node: &Node<P::PrivateKey>,
    local: &Path,
    old: Option<&Entry>,
    notes: &mut Vec<String>,
) -> Result<Option<(i64, u64)>> {
    if let Ok(meta) = fs::metadata(local) {
        match old {
            Some(old) if old.revision == node.revision && local_matches(&meta, old) => {
                return Ok(Some((old.mtime, old.size)));
            }
            Some(old) if old.revision == node.revision => {
                // Edited locally, unchanged remotely: keep tracking; the push step uploads it.
                return Ok(Some((old.mtime, old.size)));
            }
            Some(old) if !local_matches(&meta, old) => {
                let s = format!("conflict: {} edited locally and remotely; keeping local, not pushing", local.display());
                eprintln!("{s}");
                notes.push(s);
                return Ok(None);
            }
            None => {
                let s = format!("conflict: {} exists locally and remotely; keeping local, not pushing", local.display());
                eprintln!("{s}");
                notes.push(s);
                return Ok(None);
            }
            _ => {} // known file, unchanged locally, new revision remotely: refresh it
        }
    }
    let tmp = local.with_file_name(format!(".{}{PART_SUFFIX}", local.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut out = std::io::BufWriter::new(fs::File::create(&tmp)?);
    let size = drive.download(node, &mut out).await?;
    std::io::Write::flush(&mut out)?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    let mtime = node.modify_time;
    file.set_modified(UNIX_EPOCH + Duration::from_secs(mtime.max(0) as u64))?;
    drop(file);
    fs::rename(&tmp, local)?;
    println!("fetched {} ({size} bytes)", local.display());
    Ok(Some((mtime, size)))
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A local file is "ours and untouched" when size and mtime still match what we recorded.
pub fn local_matches(meta: &fs::Metadata, entry: &Entry) -> bool {
    meta.len() == entry.size && mtime_of(meta) == entry.mtime
}

/// Names come from the server decrypted but untrusted; never let one escape the folder.
fn safe_name(name: &str) -> Option<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("a.txt"), Some("a.txt"));
        for bad in ["", ".", "..", "a/b", "x\0"] {
            assert!(safe_name(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn matches_only_untouched() {
        let dir = std::env::temp_dir().join(format!("kpdrive-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f");
        fs::write(&f, b"abc").unwrap();
        let file = fs::File::options().write(true).open(&f).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
        drop(file);
        let entry = Entry { path: "f".into(), is_folder: false, revision: None, mtime: 1_700_000_000, size: 3 };
        assert!(local_matches(&fs::metadata(&f).unwrap(), &entry));
        let state = State { root: dir.clone(), event_id: None, nodes: BTreeMap::from([("id".to_string(), entry.clone())]) };
        assert!(!local_changed(&state));
        fs::write(&f, b"abcd").unwrap();
        assert!(!local_matches(&fs::metadata(&f).unwrap(), &entry));
        assert!(local_changed(&state));
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! Two-way sync between Drive and a local folder.
//!
//! Proton asks clients to sync from events rather than poll the tree, so a
//! pass only walks the remote tree when the event stream reports a change or
//! the local folder differs from what we last wrote.
//! ponytail: full walk on any change; enumerate events and touch only the
//! affected nodes when the tree gets big enough for the walk to hurt.
//!
//! Rules, in order of who wins:
//! - A file edited on one side only is copied to the other side.
//! - A file edited on both sides keeps *both*: the remote version takes the
//!   name and the local version is moved aside as a "conflict copy", which the
//!   push step then uploads. Nothing is ever silently overwritten or dropped.
//! - A file deleted locally that is unchanged remotely is trashed remotely.
//!   If it changed remotely, the remote version is restored instead.
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

            // Gone locally: the user deleted it. Trash it remotely, unless the
            // remote also changed — then the remote version comes back instead,
            // because a delete must not discard someone else's edit.
            if let Some(old) = old {
                if !local.exists() {
                    if old.revision == node.revision {
                        println!("trash {} (deleted locally)", node_rel.display());
                        to_trash.push(node.id.clone());
                        continue;
                    }
                    note!("{} was deleted locally but changed remotely; restoring the remote version", node_rel.display());
                }
            }

            let entry = if node.is_folder {
                if local.is_file() {
                    let copy = keep_local_copy(&local, "is a folder remotely", &mut notes)?;
                    note!("conflict: {} kept as {}", node_rel.display(), copy);
                }
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
                // Tracked before but missing from this pass: its download failed
                // above. Pushing now would send a half-known file, so leave it.
                None if old_paths.contains_key(&item_rel) => continue,
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

/// Brings `local` up to date with `node`. Returns the (mtime, size) recorded.
///
/// When the local file also changed, or we have no record of it at all, the
/// remote bytes are fetched first and compared: identical contents are a
/// bookkeeping gap, not a conflict, and must not spawn a copy. Only a real
/// content difference moves the local version aside.
async fn sync_file<P: PGPProviderSync>(
    drive: &mut Drive<P>,
    node: &Node<P::PrivateKey>,
    local: &Path,
    old: Option<&Entry>,
    notes: &mut Vec<String>,
) -> Result<Option<(i64, u64)>> {
    let local_meta = fs::metadata(local).ok();
    if let (Some(_), Some(old)) = (&local_meta, old) {
        if old.revision == node.revision {
            // Remote unchanged. Either nothing happened, or the user edited it;
            // returning the *recorded* mtime/size either way is what lets the
            // push step notice the edit and send it.
            return Ok(Some((old.mtime, old.size)));
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

    let mut quiet = false;
    if let Some(meta) = &local_meta {
        // Reasons the local copy might be worth keeping: it was edited while the
        // remote changed too, or we have no record of it and cannot assume.
        let why = match old {
            Some(old) if !local_matches(meta, old) => Some("edited locally and remotely"),
            None => Some("exists locally and remotely"),
            Some(_) => None, // ours, untouched: the remote update just lands
        };
        if let Some(why) = why {
            if same_contents(local, &tmp).unwrap_or(false) {
                // The bytes already agree; nothing to keep, nothing to report.
                quiet = true;
            } else {
                keep_local_copy(local, why, notes)?;
            }
        }
    }

    fs::rename(&tmp, local)?;
    if !quiet {
        println!("fetched {} ({size} bytes)", local.display());
    }
    Ok(Some((mtime, size)))
}

/// Byte-for-byte comparison, cheapest checks first.
fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut buf_a, mut buf_b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = std::io::Read::read(&mut a, &mut buf_a)?;
        if n == 0 {
            return Ok(true);
        }
        std::io::Read::read_exact(&mut b, &mut buf_b[..n])?;
        if buf_a[..n] != buf_b[..n] {
            return Ok(false);
        }
    }
}

/// Moves `local` aside so the remote version can take its name, and reports it.
/// Returns the copy's file name. The copy is untracked, so the push step in the
/// same pass uploads it as a new file.
fn keep_local_copy(local: &Path, why: &str, notes: &mut Vec<String>) -> Result<String> {
    let when = fs::metadata(local).map(|m| mtime_of(&m)).unwrap_or(0);
    let copy = conflict_copy(local, when);
    fs::rename(local, &copy).with_context(|| format!("rename {} aside", local.display()))?;
    let name = copy.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let s = format!("conflict: {} {why}; local version kept as {name}", local.display());
    eprintln!("{s}");
    notes.push(s);
    Ok(name)
}

/// `report.txt` → `report (conflict copy 2026-09-19 20-15-03).txt`, stamped with
/// the local version's mtime. Counts up rather than ever landing on a name that
/// already exists.
fn conflict_copy(local: &Path, when: i64) -> PathBuf {
    let stem = local.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = local.extension().and_then(|e| e.to_str());
    let (y, m, d, hh, mi, ss) = crate::drive::civil_utc(when);
    let stamp = format!("{y:04}-{m:02}-{d:02} {hh:02}-{mi:02}-{ss:02}");
    let mut n = 1;
    loop {
        let count = if n == 1 { String::new() } else { format!(" {n}") };
        let name = match ext {
            Some(ext) => format!("{stem} (conflict copy {stamp}{count}).{ext}"),
            None => format!("{stem} (conflict copy {stamp}{count})"),
        };
        let path = local.with_file_name(name);
        if !path.exists() {
            return path;
        }
        n += 1;
    }
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
    fn contents_comparison() {
        let dir = std::env::temp_dir().join(format!("kpdrive-eq-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b, c, d) = (dir.join("a"), dir.join("b"), dir.join("c"), dir.join("d"));
        let big = vec![7u8; 200 * 1024];
        fs::write(&a, &big).unwrap();
        fs::write(&b, &big).unwrap();
        let mut differs = big.clone();
        differs[150 * 1024] = 8;
        fs::write(&c, &differs).unwrap();
        fs::write(&d, b"short").unwrap();
        assert!(same_contents(&a, &b).unwrap());
        assert!(!same_contents(&a, &c).unwrap(), "differs past the first chunk");
        assert!(!same_contents(&a, &d).unwrap(), "different lengths");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conflict_copy_names() {
        let dir = std::env::temp_dir().join(format!("kpdrive-conflict-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("report.txt");
        let first = conflict_copy(&f, 1_700_000_000);
        assert_eq!(first.file_name().unwrap(), "report (conflict copy 2023-11-14 22-13-20).txt");
        assert_eq!(
            conflict_copy(&dir.join("notes"), 0).file_name().unwrap(),
            "notes (conflict copy 1970-01-01 00-00-00)"
        );
        // An existing copy is never overwritten.
        fs::write(&first, b"earlier").unwrap();
        assert_eq!(
            conflict_copy(&f, 1_700_000_000).file_name().unwrap(),
            "report (conflict copy 2023-11-14 22-13-20 2).txt"
        );
        fs::remove_dir_all(&dir).unwrap();
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

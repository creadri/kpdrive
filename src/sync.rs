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
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use proton_crypto::crypto::PGPProviderSync;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crate::drive::{Drive, Node};
use futures_util::{StreamExt, stream};

/// Folders listed at once during a walk.
const LIST_IN_FLIGHT: usize = 8;
/// Files of one folder uploaded together. Each upload already keeps up to six
/// block PUTs in flight, so this is for many small files, not one big one.
const UPLOAD_IN_FLIGHT: usize = 4;

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

pub const PART_SUFFIX: &str = ".kpdrive-part";

/// Patterns for paths sync should leave alone, in the root of the sync folder.
/// Same syntax as `.gitignore`, matched by the same library, so anchoring,
/// `**`, trailing-slash directory rules and `!` negation all behave as expected.
pub const IGNORE_FILE: &str = ".protonignore";

/// What the ignore file says. Absent or unreadable means nothing is ignored.
pub struct Ignores {
    matcher: Gitignore,
    root: PathBuf,
}

impl Ignores {
    pub fn load(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        // A parse error names the offending line; the rest of the file still applies.
        if let Some(e) = builder.add(root.join(IGNORE_FILE)) {
            if worth_reporting(&e) {
                crate::log::warn(&format!("{IGNORE_FILE}: {e}"));
            }
        }
        let matcher = builder.build().unwrap_or_else(|e| {
            crate::log::warn(&format!("{IGNORE_FILE} could not be read ({e}); ignoring nothing"));
            Gitignore::empty()
        });
        Self { matcher, root: root.to_owned() }
    }

    /// Whether `rel` (relative to the sync folder) is excluded, either directly
    /// or because one of its parent folders is.
    pub fn is_ignored(&self, rel: &Path, is_dir: bool) -> bool {
        if rel.as_os_str().is_empty() {
            return false;
        }
        self.matcher.matched_path_or_any_parents(self.root.join(rel), is_dir).is_ignore()
    }
}

/// Whether an ignore-file error is worth a log line. Having no ignore file is
/// the normal state, and the error for it arrives wrapped in the path, so the
/// io kind has to be dug out rather than matched on the outer variant.
fn worth_reporting(e: &ignore::Error) -> bool {
    e.io_error().map(|io| io.kind()) != Some(std::io::ErrorKind::NotFound)
}

/// What to do about a destination folder that already holds files.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Occupied {
    /// Sync into it as it stands. Identical files are adopted rather than
    /// duplicated, and anything else is uploaded.
    #[default]
    Merge,
    /// Move it aside and start from an empty folder.
    Rename,
}

impl Occupied {
    /// The wording both front ends offer, so the terminal and the window put
    /// the same choice to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Sync into it and keep what is already there",
            Self::Rename => "Move it aside and start from an empty folder",
        }
    }
}

/// What to ask before syncing into `root`, or `None` when there is nothing to
/// ask: the folder is empty, absent, or already the one being synced. kpdrive's
/// own files do not count as content, or setting the same folder twice would
/// keep asking.
pub fn folder_question(state: &State, root: &Path) -> Option<String> {
    if state.root == root && !state.nodes.is_empty() {
        return None;
    }
    let held = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name != IGNORE_FILE && !name.starts_with(".kpdrive") && !name.ends_with(PART_SUFFIX)
        })
        .count();
    if held == 0 {
        return None;
    }
    Some(format!(
        "{} already holds {held} item{}, which syncing will merge with Proton Drive.",
        root.display(),
        if held == 1 { "" } else { "s" }
    ))
}

/// Renames `root` out of the way so syncing can start from an empty folder.
/// Returns the note to show the user.
pub fn move_aside(root: &Path) -> Result<String> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "folder".into());
    let parent = root.parent().unwrap_or(Path::new("."));
    // A second run must not overwrite what the first moved aside.
    let mut aside = parent.join(format!("{name}.before-kpdrive"));
    let mut n = 2;
    while aside.exists() {
        aside = parent.join(format!("{name}.before-kpdrive-{n}"));
        n += 1;
    }
    fs::rename(root, &aside).with_context(|| format!("move {} aside to {}", root.display(), aside.display()))?;
    fs::create_dir_all(root)?;
    let note = format!("moved {} to {}", root.display(), aside.display());
    crate::log::write("WARN", &note);
    Ok(note)
}

/// Points sync at `new_root`, moving what is already synced when it can.
///
/// Recorded paths are relative to the root, so a plain rename keeps every entry
/// valid. Across filesystems a rename fails, and rather than copying gigabytes
/// this forgets what it knew and lets the next pass fetch into the new place,
/// leaving the old folder untouched for the user to delete.
pub fn set_folder(state: &mut State, new_root: PathBuf, occupied: Occupied) -> Result<String> {
    // Asked for and answered before anything else: the branches below decide
    // what to do with the old folder, and they read an emptied destination
    // differently from an occupied one.
    let mut aside = None;
    if occupied == Occupied::Rename && folder_question(state, &new_root).is_some() {
        aside = Some(move_aside(&new_root)?);
    }
    let old = std::mem::replace(&mut state.root, new_root.clone());
    let mut config = crate::config::load();
    config.sync_folder = Some(new_root.clone());
    crate::config::save(&config)?;

    if old == new_root || old.as_os_str().is_empty() {
        if aside.is_some() {
            state.nodes.clear();
        }
        fs::create_dir_all(&new_root)?;
        save_state(state)?;
        return Ok(match aside {
            Some(note) => format!("{note}; syncing to {}", new_root.display()),
            None => format!("syncing to {}", new_root.display()),
        });
    }

    let had_content = fs::read_dir(&old).map(|mut d| d.next().is_some()).unwrap_or(false);
    let note = if had_content && !new_root.exists() {
        match fs::rename(&old, &new_root) {
            Ok(()) => format!("moved {} to {}", old.display(), new_root.display()),
            Err(_) => {
                state.nodes.clear();
                format!(
                    "syncing to {} (could not move across filesystems, so it will be fetched again; {} was left alone)",
                    new_root.display(),
                    old.display()
                )
            }
        }
    } else {
        // Somewhere that already exists: the next pass compares contents and
        // adopts anything identical rather than duplicating it.
        state.nodes.clear();
        format!("syncing to {}", new_root.display())
    };

    let note = match aside {
        Some(moved) => format!("{moved}; {note}"),
        None => note,
    };
    fs::create_dir_all(&new_root)?;
    save_state(state)?;
    crate::log::write("INFO", &note);
    Ok(note)
}

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
    run_with(drive, state, force, true).await
}

/// As [`run`], but `check_local` says whether the local folder needs sweeping.
/// The daemon watches it with inotify and only sweeps when something fired,
/// on a timer, or when it cannot watch; the CLI always sweeps.
pub async fn run_with<P: PGPProviderSync>(
    drive: &mut Drive<P>,
    state: &mut State,
    force: bool,
    check_local: bool,
) -> Result<Option<Vec<String>>> {
    let mut notes: Vec<String> = Vec::new();
    fs::create_dir_all(&state.root).with_context(|| format!("create {}", state.root.display()))?;
    let ignores = Ignores::load(&state.root);
    let events = drive.events(state.event_id.as_deref()).await?;
    let local = check_local && local_changed(state, &ignores);
    if !force && !events.refresh && events.changes.is_empty() && !local {
        state.event_id = Some(events.cursor);
        save_state(state)?;
        return Ok(None);
    }
    let drive: &Drive<P> = &*drive;

    let mut nodes: HashMap<String, Node<P::PrivateKey>> = HashMap::new();
    let mut to_trash: Vec<String> = Vec::new();
    let root = drive.root()?;
    let root_id = root.id.clone();
    nodes.insert(root_id.clone(), root);

    // ---- remote → local ---------------------------------------------------
    // The event stream names what changed, so normally only those links are
    // touched. The whole tree is walked only when forced, when the cursor is
    // too old for the stream, or when an event points at a parent this pass
    // has no record of.
    let mut seen: BTreeMap<String, Entry> = state.nodes.clone();
    let mut walk = force || events.refresh;
    if !walk {
        walk = !apply_changes(drive, state, &ignores, &root_id, events.changes, &mut seen, &mut nodes, &mut to_trash, &mut notes).await?;
        if walk {
            crate::log::write("INFO", "event referred to an unknown folder; walking the tree");
        }
    }
    if walk {
        seen.clear();
        walk_tree(drive, state, &ignores, &root_id, &mut seen, &mut nodes, &mut to_trash, &mut notes).await?;
    }

    // ---- local → remote ---------------------------------------------------
    let mut by_path: HashMap<PathBuf, String> = seen.iter().map(|(id, e)| (e.path.clone(), id.clone())).collect();
    by_path.insert(PathBuf::new(), root_id.clone());
    let old_paths: HashMap<&PathBuf, &String> = state.nodes.iter().map(|(id, e)| (&e.path, id)).collect();

    let mut queue = VecDeque::from([PathBuf::new()]);
    let mut uploads: Vec<(PathBuf, String, String, Option<String>, PathBuf, i64, u64)> = Vec::new();
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
            if ignores.is_ignored(&item_rel, meta.is_dir()) {
                continue;
            }
            if meta.is_dir() {
                if !by_path.contains_key(&item_rel) {
                    let parent_id = by_path[&rel].clone();
                    if !ensure_node(drive, &mut nodes, &parent_id).await? {
                        note(&mut notes, format!("error: push {}/: its parent folder is gone remotely", item_rel.display()));
                        continue;
                    }
                    match drive.create_folder(&nodes[&parent_id], name).await {
                        Ok(node) => {
                            crate::log::info(&format!("pushed {}/", item_rel.display()));
                            seen.insert(node.id.clone(), Entry { path: item_rel.clone(), is_folder: true, revision: None, mtime: 0, size: 0 });
                            by_path.insert(item_rel.clone(), node.id.clone());
                            nodes.insert(node.id.clone(), node);
                        }
                        Err(e) => {
                            note(&mut notes, format!("error: push {}/: {e:#}", item_rel.display()));
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
            let (mut existing, changed) = match by_path.get(&item_rel) {
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
            if !ensure_node(drive, &mut nodes, &parent_id).await? {
                note(&mut notes, format!("error: push {}: its parent folder is gone remotely", item_rel.display()));
                continue;
            }
            // An edited file whose remote copy vanished meanwhile goes up as new.
            if let Some(id) = &existing {
                if !ensure_node(drive, &mut nodes, id).await? {
                    existing = None;
                }
            }
            uploads.push((item_rel, name.to_owned(), parent_id, existing, item.path(), mtime, meta.len()));
        }
        // Each file is a draft, a prepare, a PUT and a seal in sequence, so a
        // few small files in flight together cost what one did alone.
        let results: Vec<_> = {
            let nodes = &nodes;
            stream::iter(uploads.drain(..))
                .map(|(item_rel, name, parent_id, existing, path, mtime, size)| async move {
                    let existing_node = existing.as_ref().map(|id| &nodes[id]);
                    let r = match fs::File::open(&path) {
                        Ok(mut f) => drive.upload(&nodes[&parent_id], &name, existing_node, &mut f, mtime).await,
                        Err(e) => Err(anyhow::Error::from(e).context("open")),
                    };
                    (item_rel, mtime, size, r)
                })
                .buffer_unordered(UPLOAD_IN_FLIGHT)
                .collect()
                .await
        };
        for (item_rel, mtime, size, r) in results {
            match r {
                Ok((id, revision)) => {
                    crate::log::info(&format!("pushed {} ({size} bytes)", item_rel.display()));
                    seen.insert(id.clone(), Entry { path: item_rel.clone(), is_folder: false, revision: Some(revision), mtime, size });
                    by_path.insert(item_rel, id);
                }
                Err(e) => note(&mut notes, format!("error: push {}: {e:#}", item_rel.display())),
            }
        }
    }

    if let Err(e) = drive.trash(&to_trash).await {
        note(&mut notes, format!("error: trash: {e:#}"));
    }

    // ---- gone remotely (after a walk): remove locally, but only what we wrote
    // and the user hasn't touched. The event path handles its own removals.
    if walk {
        for (id, old) in &state.nodes {
            if seen.contains_key(id) || to_trash.contains(id) || ignores.is_ignored(&old.path, old.is_folder) {
                continue;
            }
            remove_local(state, old, &mut notes);
        }
    }

    state.nodes = seen;
    state.event_id = Some(events.cursor);
    save_state(state)?;
    Ok(Some(notes))
}

fn note(notes: &mut Vec<String>, s: String) {
    crate::log::warn(&s);
    notes.push(s);
}

/// Applies one batch of events. Returns `false` when something in it cannot
/// be placed, in which case the caller walks the tree instead.
#[allow(clippy::too_many_arguments)]
async fn apply_changes<P: PGPProviderSync>(
    drive: &Drive<P>,
    state: &State,
    ignores: &Ignores,
    root_id: &str,
    changes: Vec<crate::drive::Change>,
    seen: &mut BTreeMap<String, Entry>,
    nodes: &mut HashMap<String, Node<P::PrivateKey>>,
    to_trash: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> Result<bool> {
    let mut fetch: Vec<String> = Vec::new();
    let mut touched: HashSet<String> = HashSet::new();
    // Ids that left the tracked set this pass; their children go with them.
    let mut dropped: HashSet<String> = HashSet::new();
    for change in changes {
        touched.insert(change.link_id.clone());
        if change.gone {
            remove_gone(state, seen, &change.link_id, &mut dropped, notes);
        } else {
            fetch.push(change.link_id);
        }
    }
    // Deleted locally and untouched remotely: trash it. A folder's children
    // go with it, so only the topmost missing path is sent.
    let mut missing: Vec<(String, PathBuf)> = seen
        .iter()
        .filter(|(id, e)| !touched.contains(*id) && !state.root.join(&e.path).exists())
        .map(|(id, e)| (id.clone(), e.path.clone()))
        .collect();
    missing.sort_by(|a, b| a.1.cmp(&b.1));
    let mut trashed_paths: Vec<PathBuf> = Vec::new();
    for (id, path) in missing {
        seen.remove(&id);
        if trashed_paths.iter().any(|p| path.starts_with(p)) {
            dropped.insert(id);
            continue;
        }
        crate::log::info(&format!("trash {} (deleted locally)", path.display()));
        to_trash.push(id.clone());
        dropped.insert(id);
        trashed_paths.push(path);
    }

    let mut pending: Vec<Node<P::PrivateKey>> = Vec::new();
    for (id, node) in drive.nodes_by_ids(&fetch).await? {
        match node {
            Some(n) => pending.push(n),
            // Gone between the event and now.
            None => remove_gone(state, seen, &id, &mut dropped, notes),
        }
    }
    // Events are not ordered parent-first, so apply in rounds: a node waits
    // until its parent is placed or dropped. Anything still waiting at the end
    // has a parent this pass knows nothing about, and only a walk can place it.
    while !pending.is_empty() {
        let before = pending.len();
        let mut waiting = Vec::new();
        for node in pending {
            let parent = node.parent_id.as_deref().unwrap_or("");
            let parent_rel = if parent == root_id {
                PathBuf::new()
            } else if let Some(e) = seen.get(parent).filter(|e| e.is_folder) {
                e.path.clone()
            } else if dropped.contains(parent) {
                seen.remove(&node.id);
                dropped.insert(node.id.clone());
                continue;
            } else {
                waiting.push(node);
                continue;
            };
            let Some(name) = safe_name(&node.name) else {
                crate::log::warn(&format!("skip: unsafe name {:?}", node.name));
                continue;
            };
            let node_rel = parent_rel.join(name);
            if ignores.is_ignored(&node_rel, node.is_folder) {
                seen.remove(&node.id);
                continue;
            }
            let old = seen.get(&node.id).cloned();
            match apply_node(drive, state, &node, node_rel, old.as_ref(), to_trash, notes).await? {
                Some(entry) => {
                    // A folder moved on disk took its subtree with it; keep the
                    // recorded paths of everything under it in step.
                    if let (true, Some(old)) = (node.is_folder, &old) {
                        if old.path != entry.path {
                            for e in seen.values_mut() {
                                if let Ok(rest) = e.path.strip_prefix(&old.path) {
                                    e.path = entry.path.join(rest);
                                }
                            }
                        }
                    }
                    seen.insert(node.id.clone(), entry);
                }
                None => {
                    // Trashed, or a conflict kept: nothing under it is tracked either.
                    if let Some(old) = &old {
                        if old.is_folder {
                            let under: Vec<String> = seen.iter().filter(|(_, e)| e.path.starts_with(&old.path)).map(|(k, _)| k.clone()).collect();
                            for k in under {
                                seen.remove(&k);
                                dropped.insert(k);
                            }
                        }
                    }
                    seen.remove(&node.id);
                    dropped.insert(node.id.clone());
                }
            }
            nodes.insert(node.id.clone(), node);
        }
        if waiting.len() == before {
            return Ok(false);
        }
        pending = waiting;
    }
    Ok(true)
}

/// The full tree, breadth first: every folder on one level is listed at once,
/// then its children are handled in order.
#[allow(clippy::too_many_arguments)]
async fn walk_tree<P: PGPProviderSync>(
    drive: &Drive<P>,
    state: &State,
    ignores: &Ignores,
    root_id: &str,
    seen: &mut BTreeMap<String, Entry>,
    nodes: &mut HashMap<String, Node<P::PrivateKey>>,
    to_trash: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> Result<()> {
    let mut level = vec![(root_id.to_owned(), PathBuf::new())];
    while !level.is_empty() {
        let listed: Vec<Result<Vec<Node<P::PrivateKey>>>> = {
            let nodes = &*nodes;
            stream::iter(level.iter())
                .map(|(id, _)| async move { drive.list(&nodes[id]).await })
                .buffered(LIST_IN_FLIGHT)
                .collect()
                .await
        };
        let mut next_level = Vec::new();
        for ((_, rel), children) in level.into_iter().zip(listed) {
            for node in children? {
                let Some(name) = safe_name(&node.name) else {
                    crate::log::warn(&format!("skip: unsafe name {:?}", node.name));
                    continue;
                };
                let node_rel = rel.join(name);
                if ignores.is_ignored(&node_rel, node.is_folder) {
                    continue;
                }
                let old = state.nodes.get(&node.id).cloned();
                if let Some(entry) = apply_node(drive, state, &node, node_rel.clone(), old.as_ref(), to_trash, notes).await? {
                    if node.is_folder {
                        next_level.push((node.id.clone(), node_rel));
                    }
                    seen.insert(node.id.clone(), entry);
                }
                nodes.insert(node.id.clone(), node);
            }
        }
        level = next_level;
    }
    Ok(())
}

/// What one remote node means for the local folder: a move to apply, a
/// local deletion to propagate, a folder to create, or a file to bring up to
/// date. Returns the entry to record, or `None` when the node is not tracked
/// this round (trashed, conflict kept, or failed).
async fn apply_node<P: PGPProviderSync>(
    drive: &Drive<P>,
    state: &State,
    node: &Node<P::PrivateKey>,
    node_rel: PathBuf,
    old: Option<&Entry>,
    to_trash: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> Result<Option<Entry>> {
    let local = state.root.join(&node_rel);

    // Same content under a new name or parent: rename instead of re-fetching.
    if let Some(old) = old {
        let old_local = state.root.join(&old.path);
        if old.path != node_rel && old.revision == node.revision && old_local.exists() && !local.exists() {
            fs::rename(&old_local, &local).with_context(|| format!("rename {}", old_local.display()))?;
            crate::log::info(&format!("moved {} -> {}", old.path.display(), node_rel.display()));
        }
    }

    // Gone locally: the user deleted it. Trash it remotely, unless the remote
    // also changed — then the remote version comes back instead, because a
    // delete must not discard someone else's edit.
    if let Some(old) = old {
        if !local.exists() {
            if old.revision == node.revision {
                crate::log::info(&format!("trash {} (deleted locally)", node_rel.display()));
                to_trash.push(node.id.clone());
                return Ok(None);
            }
            note(notes, format!("{} was deleted locally but changed remotely; restoring the remote version", node_rel.display()));
        }
    }

    if node.is_folder {
        if local.is_file() {
            let copy = keep_local_copy(&local, "is a folder remotely", notes)?;
            note(notes, format!("conflict: {} kept as {}", node_rel.display(), copy));
        }
        fs::create_dir_all(&local)?;
        return Ok(Some(Entry { path: node_rel, is_folder: true, revision: None, mtime: 0, size: 0 }));
    }
    match sync_file(drive, node, &local, old, notes).await {
        Ok(Some((mtime, size))) => Ok(Some(Entry { path: node_rel, is_folder: false, revision: node.revision.clone(), mtime, size })),
        Ok(None) => Ok(None), // conflict: kept local, not tracked this round
        Err(e) => {
            note(notes, format!("error: {}: {e:#}", node_rel.display()));
            Ok(None)
        }
    }
}

/// A link the events say is trashed or deleted: forget it, and remove what we
/// wrote for it if the user left it alone. A folder takes its recorded
/// subtree with it, files first, then whatever directories are empty.
fn remove_gone(state: &State, seen: &mut BTreeMap<String, Entry>, id: &str, dropped: &mut HashSet<String>, notes: &mut Vec<String>) {
    let Some(old) = seen.remove(id) else { return };
    dropped.insert(id.to_owned());
    if !old.is_folder {
        remove_local(state, &old, notes);
        return;
    }
    let under: Vec<(String, Entry)> = seen
        .iter()
        .filter(|(_, e)| e.path.starts_with(&old.path))
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    let mut dirs = vec![old.path.clone()];
    for (child_id, e) in under {
        seen.remove(&child_id);
        dropped.insert(child_id);
        if e.is_folder {
            dirs.push(e.path);
        } else {
            remove_local(state, &e, notes);
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let local = state.root.join(&d);
        match fs::remove_dir(&local) {
            Ok(()) => crate::log::info(&format!("removed {}/", d.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => note(notes, format!("keep: {} removed remotely but not empty locally", d.display())),
        }
    }
}

/// Deletes one recorded path locally, only if it is still exactly what we wrote.
fn remove_local(state: &State, old: &Entry, notes: &mut Vec<String>) {
    let local = state.root.join(&old.path);
    let Ok(meta) = fs::metadata(&local) else { return };
    if old.is_folder {
        if fs::read_dir(&local).map(|mut d| d.next().is_none()).unwrap_or(false) {
            let _ = fs::remove_dir(&local);
            crate::log::info(&format!("removed {}/", old.path.display()));
        } else {
            note(notes, format!("keep: {} removed remotely but not empty locally", old.path.display()));
        }
    } else if local_matches(&meta, old) {
        let _ = fs::remove_file(&local);
        crate::log::info(&format!("removed {}", old.path.display()));
    } else {
        note(notes, format!("keep: {} removed remotely but edited locally", old.path.display()));
    }
}

/// Makes sure `nodes` holds the node with `id`, from the folder cache or a
/// fetch. `false` means it no longer exists remotely.
async fn ensure_node<P: PGPProviderSync>(
    drive: &Drive<P>,
    nodes: &mut HashMap<String, Node<P::PrivateKey>>,
    id: &str,
) -> Result<bool> {
    if nodes.contains_key(id) {
        return Ok(true);
    }
    let fetched = match drive.cached_folder(id) {
        Some(n) => Some(n),
        None => drive.nodes_by_ids(std::slice::from_ref(&id.to_owned())).await?.into_iter().next().and_then(|(_, n)| n),
    };
    match fetched {
        Some(n) => {
            nodes.insert(id.to_owned(), n);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Cheap local scan: anything new, edited or deleted since the last pass?
fn local_changed(state: &State, ignores: &Ignores) -> bool {
    let by_path: HashMap<&PathBuf, &Entry> = state.nodes.values().map(|e| (&e.path, e)).collect();
    for entry in state.nodes.values() {
        if !ignores.is_ignored(&entry.path, entry.is_folder) && !state.root.join(&entry.path).exists() {
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
            // An ignored file must not keep waking the daemon.
            if ignores.is_ignored(&item_rel, meta.is_dir()) {
                continue;
            }
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
    drive: &Drive<P>,
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
        crate::log::info(&format!("fetched {} ({size} bytes)", local.display()));
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
    crate::log::warn(&s);
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
    fn missing_ignore_file_is_not_worth_a_warning() {
        let dir = std::env::temp_dir().join(format!("kpdrive-noign-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let e = GitignoreBuilder::new(&dir).add(dir.join(IGNORE_FILE)).expect("absent file errors");
        assert!(!worth_reporting(&e), "absent ignore file logged as: {e}");
        let bad = ignore::Error::Glob { glob: Some("[".into()), err: "unclosed".into() };
        assert!(worth_reporting(&bad), "a real parse error must still be logged");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn asks_only_when_the_folder_holds_someone_elses_files() {
        let dir = std::env::temp_dir().join(format!("kpdrive-occ-{}", std::process::id()));
        let root = dir.join("dest");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&root).unwrap();
        let mut state = State { root: root.clone(), ..State::default() };

        assert!(folder_question(&state, &root).is_none(), "an empty folder needs no question");
        assert!(folder_question(&state, &dir.join("absent")).is_none(), "nor does one that is not there");

        fs::write(root.join(IGNORE_FILE), "*.tmp\n").unwrap();
        fs::write(root.join(format!("half-done{PART_SUFFIX}")), "").unwrap();
        assert!(folder_question(&state, &root).is_none(), "our own files are not the user's");

        fs::write(root.join("theirs.txt"), "hello").unwrap();
        let question = folder_question(&state, &root).expect("a file of theirs is worth asking about");
        assert!(question.contains("1 item"), "{question}");

        state.nodes.insert("id".into(), Entry { path: PathBuf::from("theirs.txt"), is_folder: false, revision: None, mtime: 0, size: 5 });
        assert!(folder_question(&state, &root).is_none(), "a folder already being synced is not a question");

        // Moving aside leaves an empty folder behind and never overwrites.
        state.nodes.clear();
        move_aside(&root).unwrap();
        assert!(root.exists() && fs::read_dir(&root).unwrap().next().is_none());
        assert!(dir.join("dest.before-kpdrive").join("theirs.txt").exists());
        fs::write(root.join("again.txt"), "x").unwrap();
        move_aside(&root).unwrap();
        assert!(dir.join("dest.before-kpdrive-2").join("again.txt").exists(), "the first move is kept");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ignore_rules() {
        let dir = std::env::temp_dir().join(format!("kpdrive-ignore-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(IGNORE_FILE),
            "*.tmp\nbuild/\n!keep.tmp\n**/node_modules/\n/only-at-root.txt\n",
        )
        .unwrap();
        let ig = Ignores::load(&dir);
        assert!(ig.is_ignored(Path::new("a.tmp"), false));
        assert!(ig.is_ignored(Path::new("deep/inside/a.tmp"), false), "patterns are not anchored");
        assert!(!ig.is_ignored(Path::new("keep.tmp"), false), "negation wins by being last");
        assert!(ig.is_ignored(Path::new("build"), true), "trailing slash means the folder");
        assert!(ig.is_ignored(Path::new("build/out.o"), false), "and everything under it");
        assert!(ig.is_ignored(Path::new("web/node_modules"), true));
        assert!(ig.is_ignored(Path::new("only-at-root.txt"), false));
        assert!(!ig.is_ignored(Path::new("sub/only-at-root.txt"), false), "leading slash anchors");
        assert!(!ig.is_ignored(Path::new("notes.txt"), false));
        assert!(!ig.is_ignored(Path::new(""), true), "the root is never ignored");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_is_ignored_without_the_file() {
        let dir = std::env::temp_dir().join(format!("kpdrive-noignore-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ig = Ignores::load(&dir);
        assert!(!ig.is_ignored(Path::new("a.tmp"), false));
        fs::remove_dir_all(&dir).unwrap();
    }

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
        let ignores = Ignores::load(&dir);
        assert!(!local_changed(&state, &ignores));
        fs::write(&f, b"abcd").unwrap();
        assert!(!local_matches(&fs::metadata(&f).unwrap(), &entry));
        assert!(local_changed(&state, &ignores));
        fs::remove_dir_all(&dir).unwrap();
    }
}

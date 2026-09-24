//! Download-only sync of the Proton Photos timeline.
//!
//! Photos live in their own volume with their own share, and the timeline is a
//! flat, capture-time-ordered list rather than a folder tree. They are filed
//! locally under `<dest>/YYYY/MM/`, which is what every photo tool expects and
//! what keeps a large library navigable.
//!
//! ponytail: download only. Uploading a photo means generating a thumbnail and
//! reading EXIF for the capture time, which needs an image decoder; add that
//! when someone actually wants to push photos up from the desktop.

use anyhow::{Context, Result, bail};
use proton_crypto::crypto::PGPProviderSync;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crate::drive::{Drive, civil_utc};

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub dest: PathBuf,
    /// link id → what we wrote for it.
    pub photos: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    /// Relative to `State::dest`.
    pub path: PathBuf,
    pub revision: Option<String>,
}

pub fn state_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&std::env::var_os("HOME").expect("HOME")).join(".local/share"));
    base.join("kpdrive/photos.json")
}

pub fn load_state() -> Result<Option<State>> {
    match fs::read(state_path()) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).context("photos.json is corrupt")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("read photos.json"),
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

/// Where photos go: the setting, what an earlier version recorded, or the
/// default. Recording it in the photo state came first; the setting is where
/// it belongs, and an existing folder is adopted rather than re-downloaded.
pub fn dest() -> Result<PathBuf> {
    if let Some(dir) = crate::config::load().photos_sync_folder.filter(|d| !d.as_os_str().is_empty()) {
        return Ok(dir);
    }
    if let Some(old) = load_state()?.map(|s| s.dest).filter(|d| !d.as_os_str().is_empty()) {
        return Ok(old);
    }
    default_dest()
}

/// Points the photo download at `dir` from now on.
pub fn set_dest(dir: PathBuf) -> Result<()> {
    let mut config = crate::config::load();
    config.photos_sync_folder = Some(dir);
    crate::config::save(&config)
}

/// `~/Pictures/ProtonDrive`, honouring a localized or relocated Pictures folder.
pub fn default_dest() -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME not set")?);
    let pictures = std::process::Command::new("xdg-user-dir")
        .arg("PICTURES")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute() && p != &home)
        .unwrap_or_else(|| home.join("Pictures"));
    Ok(pictures.join("ProtonDrive"))
}

/// Downloads every timeline photo not already on disk. Returns how many arrived.
/// Brings the timeline down to where it belongs, loading and saving the
/// photo state around it. `None` when the account has no photos library.
///
/// Photos are download only: this never uploads, renames or deletes anything
/// in the account, so the copy on disk is a copy and nothing more.
pub async fn pass<P: PGPProviderSync>(drive: &Drive<P>, sync_root: Option<&Path>) -> Result<Option<(usize, PathBuf)>> {
    let mut state = load_state()?.unwrap_or_default();
    state.dest = dest()?;
    check_dest(&state.dest, sync_root)?;
    let fetched = run(drive, &mut state).await?;
    save_state(&state)?;
    Ok(fetched.map(|n| (n, state.dest)))
}

pub async fn run<P: PGPProviderSync>(drive: &Drive<P>, state: &mut State) -> Result<Option<usize>> {
    let Some(root) = drive.photos_root().await? else {
        return Ok(None);
    };
    fs::create_dir_all(&state.dest).with_context(|| format!("create {}", state.dest.display()))?;

    let mut wanted = drive.timeline(&root).await?;
    // Oldest first, so an interrupted run resumes somewhere sensible and the
    // folders fill in chronological order. Dedupe: overlapping pages would
    // otherwise fetch the same photo twice.
    wanted.sort_by(|a, b| a.capture_time.cmp(&b.capture_time).then_with(|| a.id.cmp(&b.id)));
    wanted.dedup_by(|a, b| a.id == b.id);

    let missing: Vec<_> = wanted
        .iter()
        .filter(|p| match state.photos.get(&p.id) {
            Some(entry) => !state.dest.join(&entry.path).exists(),
            None => true,
        })
        .collect();
    if missing.is_empty() {
        return Ok(Some(0));
    }
    crate::log::info(&format!("{} photo(s) to fetch", missing.len()));

    let mut fetched = 0;
    for chunk in missing.chunks(150) {
        let ids: Vec<String> = chunk.iter().map(|p| p.id.clone()).collect();
        let capture: BTreeMap<&str, i64> = chunk.iter().map(|p| (p.id.as_str(), p.capture_time)).collect();
        let nodes = drive.photo_nodes(&root, &ids).await?;
        for node in nodes {
            let taken = capture.get(node.id.as_str()).copied().unwrap_or(node.modify_time);
            let rel = match state.photos.get(&node.id) {
                // Known photo whose file went missing: put it back where it was.
                Some(entry) if entry.revision == node.revision => entry.path.clone(),
                _ => match dated_path(&state.dest, taken, &node.name) {
                    Some(rel) => rel,
                    None => {
                        crate::log::warn(&format!("skip: unusable photo name {:?}", node.name));
                        continue;
                    }
                },
            };
            let local = state.dest.join(&rel);
            if let Some(parent) = local.parent() {
                fs::create_dir_all(parent)?;
            }
            match fetch(drive, &node, &local, taken).await {
                Ok(size) => {
                    crate::log::info(&format!("fetched photo {} ({size} bytes)", rel.display()));
                    state.photos.insert(node.id.clone(), Entry { path: rel, revision: node.revision.clone() });
                    // Per photo: one recorded late would come back as `name (2)`
                    // after an interrupted run, next to the copy already here.
                    save_state(state)?;
                    fetched += 1;
                }
                Err(e) => crate::log::error(&format!("error: {}: {e:#}", rel.display())),
            }
        }
    }
    Ok(Some(fetched))
}

async fn fetch<P: PGPProviderSync>(
    drive: &Drive<P>,
    node: &crate::drive::Node<P::PrivateKey>,
    local: &Path,
    taken: i64,
) -> Result<u64> {
    let tmp = local.with_extension("kpdrive-part");
    let mut out = std::io::BufWriter::new(fs::File::create(&tmp)?);
    let size = match drive.download(node, &mut out).await {
        Ok(size) => size,
        Err(e) => {
            drop(out);
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    std::io::Write::flush(&mut out)?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    // Capture time, not upload time: that is what photo tools sort by.
    file.set_modified(UNIX_EPOCH + Duration::from_secs(taken.max(0) as u64))?;
    drop(file);
    fs::rename(&tmp, local)?;
    Ok(size)
}

/// `YYYY/MM/name`, counting up if that name is taken by a different photo.
fn dated_path(dest: &Path, taken: i64, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return None;
    }
    let (y, m, ..) = civil_utc(taken);
    let dir = PathBuf::from(format!("{y:04}")).join(format!("{m:02}"));
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    };
    let mut n = 1;
    loop {
        let candidate = match (n, ext) {
            (1, _) => name.to_owned(),
            (_, Some(ext)) => format!("{stem} ({n}).{ext}"),
            (_, None) => format!("{stem} ({n})"),
        };
        let rel = dir.join(candidate);
        if !dest.join(&rel).exists() {
            return Some(rel);
        }
        n += 1;
    }
}

/// Refuses a destination inside the file sync folder: the file sync would treat
/// every photo as a new local file and upload the whole library into Drive.
pub fn check_dest(dest: &Path, sync_root: Option<&Path>) -> Result<()> {
    if let Some(root) = sync_root {
        if dest == root || dest.starts_with(root) {
            bail!(
                "{} is inside the file sync folder ({}); photos there would be uploaded back into Drive. Pick another --dest.",
                dest.display(),
                root.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_dated_and_unique() {
        let dir = std::env::temp_dir().join(format!("kpdrive-photos-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let taken = 1_700_000_000; // 2023-11-14
        assert_eq!(dated_path(&dir, taken, "IMG_1.jpg").unwrap(), Path::new("2023/11/IMG_1.jpg"));
        assert_eq!(dated_path(&dir, taken, "noext").unwrap(), Path::new("2023/11/noext"));
        fs::create_dir_all(dir.join("2023/11")).unwrap();
        fs::write(dir.join("2023/11/IMG_1.jpg"), b"x").unwrap();
        assert_eq!(dated_path(&dir, taken, "IMG_1.jpg").unwrap(), Path::new("2023/11/IMG_1 (2).jpg"));
        for bad in ["", "..", "a/b.jpg"] {
            assert!(dated_path(&dir, taken, bad).is_none(), "{bad:?}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dest_must_be_outside_the_sync_folder() {
        let root = Path::new("/home/me/ProtonDrive");
        assert!(check_dest(Path::new("/home/me/Pictures/Proton Drive"), Some(root)).is_ok());
        assert!(check_dest(Path::new("/home/me/ProtonDrive"), Some(root)).is_err());
        assert!(check_dest(Path::new("/home/me/ProtonDrive/Photos"), Some(root)).is_err());
        assert!(check_dest(Path::new("/home/me/Pictures"), None).is_ok());
    }
}

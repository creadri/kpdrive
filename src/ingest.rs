//! Photo ingestion: anything dropped into `photos_ingestion_folder` goes up
//! into Proton Photos, and the local file is then trashed, or deleted outright
//! with `photos_ingestion_perm_rm`.
//!
//! Proton Photos decides where a photo sits in the timeline, so the only
//! operation offered is adding one; see docs/photos-plan.md.

use anyhow::{Context, Result, bail, ensure};
use proton_crypto::crypto::PGPProviderSync;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::drive::{Drive, PhotoMeta};

/// A file already in Proton Photos, by what it looked like at the time.
/// Written before the local file is removed, so a pass that dies in between
/// removes it next time instead of uploading it again.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Ingested {
    pub size: u64,
    pub mtime: i64,
}

/// Absolute path → what went up from it.
type Record = BTreeMap<PathBuf, Ingested>;

/// A failed file is tried again after this long, not every pass.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

/// What a pass remembers for the next one.
#[derive(Default)]
pub struct Seen {
    /// Each file's (size, mtime) at the previous look.
    looks: HashMap<PathBuf, (u64, i64)>,
    /// Files that failed, and when.
    failed: HashMap<PathBuf, Instant>,
}

fn record_path() -> PathBuf {
    crate::photos::state_path().with_file_name("ingest.json")
}

fn load_record() -> Result<Record> {
    match fs::read(record_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("ingest.json is corrupt"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::new()),
        Err(e) => Err(e).context("read ingest.json"),
    }
}

fn save_record(record: &Record) -> Result<()> {
    let path = record_path();
    fs::create_dir_all(path.parent().expect("record path has a parent"))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// The ingestion folder, when one is set.
pub fn folder() -> Option<PathBuf> {
    crate::config::load().photos_ingestion_folder.filter(|d| !d.as_os_str().is_empty())
}

/// Refuses an ingestion folder that overlaps the sync folder or the photos
/// download: two mechanisms would fight over the same files, and the download
/// would be uploaded straight back.
pub fn check_folder(folder: &Path, others: &[&Path]) -> Result<()> {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    let folder = canon(folder);
    for other in others.iter().map(|o| canon(o)) {
        if folder.starts_with(&other) || other.starts_with(&folder) {
            bail!("the photo ingestion folder {} overlaps {}; pick a folder of its own", folder.display(), other.display());
        }
    }
    Ok(())
}

/// The MIME type Proton Photos is told, or `None` for a file that is not a
/// photo or video and stays where it is.
pub fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "tif" | "tiff" => "image/tiff",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => return None,
    })
}

/// When the photo was taken, unix seconds: EXIF `DateTimeOriginal` with its
/// recorded offset, else read in the local zone as every photo tool does,
/// else `None` and the caller uses the modified time.
pub fn capture_time(path: &Path) -> Option<i64> {
    use exif::{In, Tag, Value};
    let file = fs::File::open(path).ok()?;
    let exif = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)).ok()?;
    let ascii = |tag| match exif.get_field(tag, In::PRIMARY).map(|f| &f.value) {
        Some(Value::Ascii(v)) => v.first().cloned(),
        _ => None,
    };
    let raw = ascii(Tag::DateTimeOriginal).or_else(|| ascii(Tag::DateTime))?;
    let mut when = exif::DateTime::from_ascii(&raw).ok()?;
    if let Some(offset) = ascii(Tag::OffsetTimeOriginal).or_else(|| ascii(Tag::OffsetTime)) {
        let _ = when.parse_offset(&offset);
    }
    local_to_unix(&when)
}

/// ponytail: GNU `date` does the local-zone arithmetic, DST included, rather
/// than a time zone crate; swap in `jiff` if this ever runs somewhere without it.
fn local_to_unix(when: &exif::DateTime) -> Option<i64> {
    let mut text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        when.year, when.month, when.day, when.hour, when.minute, when.second
    );
    if let Some(minutes) = when.offset {
        let sign = if minutes < 0 { '-' } else { '+' };
        text.push_str(&format!(" {sign}{:02}{:02}", minutes.unsigned_abs() / 60, minutes.unsigned_abs() % 60));
    }
    let out = std::process::Command::new("date").args(["-d", &text, "+%s"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn stat(path: &Path) -> Option<(u64, i64)> {
    let m = fs::symlink_metadata(path).ok().filter(|m| m.is_file())?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((m.len(), mtime))
}

/// Every photo or video under `folder`, hidden files and folders left out.
fn candidates(folder: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(folder)
        .standard_filters(false)
        .hidden(true)
        .follow_links(false)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .filter(|p| media_type(p).is_some())
        .collect()
}

/// Moves `path` to the desktop trash, or deletes it with `perm_rm`. The trash
/// goes through `gio`, which knows the per-filesystem trash directories a
/// phone mount or an SD card needs.
fn remove(path: &Path, perm_rm: bool) -> Result<()> {
    if perm_rm {
        return fs::remove_file(path).with_context(|| format!("delete {}", path.display()));
    }
    let out = std::process::Command::new("gio").arg("trash").arg("--").arg(path).output().context("run gio trash")?;
    ensure!(out.status.success(), "gio trash {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

/// One look at the ingestion folder. A file goes up once it has looked the
/// same on two consecutive looks, so a copy still in progress is left alone;
/// `seen` carries the previous look. Returns how many photos went up.
pub async fn pass<P: PGPProviderSync>(drive: &Drive<P>, seen: &mut Seen, others: &[&Path]) -> Result<usize> {
    let Some(folder) = folder() else { return Ok(0) };
    if !folder.is_dir() {
        // An unplugged card or phone: nothing to do until it is back.
        return Ok(0);
    }
    check_folder(&folder, others)?;
    let perm_rm = crate::config::load().photos_ingestion_perm_rm;
    let mut record = load_record()?;
    let files = candidates(&folder);
    seen.looks.retain(|p, _| files.contains(p));
    seen.failed.retain(|p, at| files.contains(p) && at.elapsed() < RETRY_AFTER);
    // In Proton Photos but not removed yet: an entry whose file is gone or
    // changed is stale either way.
    let before = record.len();
    record.retain(|p, e| stat(p) == Some((e.size, e.mtime)));
    if record.len() != before {
        save_record(&record)?;
    }

    let mut root = None;
    let mut uploaded = 0;
    for path in files {
        let Some(now) = stat(&path) else { continue };
        if !record.contains_key(&path) {
            // Unsettled, empty, or failed recently: leave it for now.
            if seen.looks.insert(path.clone(), now) != Some(now) || now.0 == 0 || seen.failed.contains_key(&path) {
                continue;
            }
            if root.is_none() {
                root = match drive.photos_root().await? {
                    Some(r) => Some(r),
                    None => bail!("this account has no Proton Photos library to ingest into"),
                };
            }
            match ingest_one(drive, root.as_ref().expect("set above"), &path, now.1).await {
                Ok(true) => {
                    crate::log::info(&format!("ingested {}", path.display()));
                    uploaded += 1;
                }
                Ok(false) => crate::log::info(&format!("already in Proton Photos: {}", path.display())),
                Err(e) => {
                    crate::log::error(&format!("cannot ingest {}: {e:#}", path.display()));
                    seen.failed.insert(path, Instant::now());
                    continue;
                }
            }
            seen.looks.remove(&path);
            record.insert(path.clone(), Ingested { size: now.0, mtime: now.1 });
            save_record(&record)?;
        }
        match remove(&path, perm_rm) {
            Ok(()) => {
                record.remove(&path);
                save_record(&record)?;
            }
            Err(e) => crate::log::warn(&format!("in Proton Photos but not removed: {e:#}")),
        }
    }
    Ok(uploaded)
}

/// Uploads one photo; `false` when Proton Photos already holds it.
async fn ingest_one<P: PGPProviderSync>(drive: &Drive<P>, root: &crate::drive::Node<P::PrivateKey>, path: &Path, mtime: i64) -> Result<bool> {
    let name = path.file_name().context("no file name")?.to_string_lossy().into_owned();
    let open = || fs::File::open(path).map(std::io::BufReader::new).with_context(|| format!("open {}", path.display()));
    if drive.photo_exists(root, &name, &mut open()?).await? {
        return Ok(false);
    }
    let mime = media_type(path).expect("only media is ingested");
    let (thumbnail, size) = match thumbnail(path) {
        Some((t, s)) => (Some(t), Some(s)),
        None => (None, None),
    };
    let meta = PhotoMeta {
        mime,
        capture_time: capture_time(path).unwrap_or(mtime),
        thumbnail,
        size,
        tags: if mime.starts_with("video/") { vec![2] } else { vec![] },
    };
    drive.upload_photo(root, &name, &mut open()?, mtime, &meta).await?;
    Ok(true)
}

/// A timeline tile: JPEG, at most 512 px a side, under 60 KiB once encrypted
/// (the web client aims at 90% of that), plus the size as displayed. `None`
/// for what cannot be decoded here, HEIC and video: those go up without one.
fn thumbnail(path: &Path) -> Option<(Vec<u8>, (u32, u32))> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    let size = (img.width(), img.height());
    let small = img.thumbnail(512, 512).into_rgb8();
    for quality in [70, 50, 30, 10] {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&small).ok()?;
        if out.len() < 60 * 1024 * 9 / 10 {
            return Some((out, size));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_is_taken() {
        assert_eq!(media_type(Path::new("a/IMG_1.JPG")), Some("image/jpeg"));
        assert_eq!(media_type(Path::new("clip.mov")), Some("video/quicktime"));
        assert_eq!(media_type(Path::new("notes.txt")), None);
        assert_eq!(media_type(Path::new("noext")), None);
    }

    #[test]
    fn folders_may_not_overlap() {
        let sync = Path::new("/home/me/ProtonDrive");
        let photos = Path::new("/home/me/Pictures/Proton Drive");
        assert!(check_folder(Path::new("/home/me/Pictures/Ingest"), &[sync, photos]).is_ok());
        assert!(check_folder(Path::new("/home/me/ProtonDrive/in"), &[sync, photos]).is_err());
        assert!(check_folder(Path::new("/home/me/Pictures"), &[sync, photos]).is_err());
    }

    #[test]
    fn capture_time_honours_the_offset() {
        let mut when = exif::DateTime::from_ascii(b"2024:03:15 15:17:00").unwrap();
        when.parse_offset(b"+01:00").unwrap();
        assert_eq!(local_to_unix(&when), Some(1_710_512_220));
    }
}

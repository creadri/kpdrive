//! `kpdrive setup`: the local folder, a Dolphin Places entry, and autostart.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME not set")?))
}

fn xdg(var: &str, default_rel: &str) -> Result<PathBuf> {
    Ok(std::env::var_os(var).map(PathBuf::from).unwrap_or(home()?.join(default_rel)))
}

/// Minimal percent-encoding for a file:// href.
fn href(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Adds an entry called `title` to Dolphin's Places unless one already points
/// at `root`.
pub fn places_entry(root: &Path, title: &str) -> Result<bool> {
    let file = xdg("XDG_DATA_HOME", ".local/share")?.join("user-places.xbel");
    // A profile where Dolphin has never saved a place has no file yet; start
    // one rather than failing the whole setup over it.
    let xml = match std::fs::read_to_string(&file) {
        Ok(xml) => xml,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xbel>\n<xbel xmlns:bookmark=\"http://www.freedesktop.org/standards/desktop-bookmarks\" xmlns:kdepriv=\"http://www.kde.org/kdepriv\" xmlns:mime=\"http://www.freedesktop.org/standards/shared-mime-info\">\n</xbel>\n".to_owned()
        }
        Err(e) => return Err(anyhow::Error::from(e).context(format!("read {}", file.display()))),
    };
    let href = href(root);
    if xml.contains(&format!("href=\"{href}\"")) {
        return Ok(false);
    }
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let title = title.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let entry = format!(
        " <bookmark href=\"{href}\">\n  <title>{title}</title>\n  <info>\n   <metadata owner=\"http://freedesktop.org\">\n    <bookmark:icon name=\"folder-cloud\"/>\n   </metadata>\n   <metadata owner=\"http://www.kde.org\">\n    <ID>{id}/0</ID>\n   </metadata>\n  </info>\n </bookmark>\n</xbel>"
    );
    let updated = xml.replacen("</xbel>", &entry, 1);
    if updated == xml {
        anyhow::bail!("{} has no </xbel> closing tag", file.display());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("xbel.kpdrive-tmp");
    std::fs::write(&tmp, updated)?;
    std::fs::rename(&tmp, &file)?;
    Ok(true)
}

/// Starts `kpdrive sync --watch` with the Plasma session.
pub fn autostart() -> Result<PathBuf> {
    let dir = xdg("XDG_CONFIG_HOME", ".config")?.join("autostart");
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    let file = dir.join("kpdrive.desktop");
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=Proton Drive (kpdrive)\nComment=Sync Proton Drive with ~/ProtonDrive\nComment[fr]=Synchroniser Proton Drive avec ~/ProtonDrive\nExec={} sync --watch\nIcon=folder-cloud\nTerminal=false\nX-KDE-autostart-after=panel\n",
            exe.display()
        ),
    )?;
    Ok(file)
}

/// Drops a commented `.protonignore` into the sync folder when there is none,
/// so the feature is discoverable. Returns whether one was written.
pub fn ignore_template(root: &Path) -> Result<bool> {
    let file = root.join(crate::sync::IGNORE_FILE);
    if file.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(root)?;
    std::fs::write(
        &file,
        "# Paths kpdrive leaves alone, one pattern per line.\n\
         # Same syntax as .gitignore:\n\
         #   *.tmp           any file with that extension, at any depth\n\
         #   build/          a folder and everything in it\n\
         #   /Scratch/       only at the top of this folder\n\
         #   !keep.tmp       an exception to an earlier pattern\n\
         #\n\
         # Ignored paths are neither uploaded nor downloaded, and are never\n\
         # deleted by sync. This file itself does sync, like .gitignore does.\n",
    )?;
    Ok(true)
}

/// True when the package already installed this file system-wide, in which case
/// writing a copy under the user's home would only shadow it with a stale path.
fn packaged(rel: &str) -> Option<PathBuf> {
    let system = Path::new("/usr/share").join(rel);
    system.is_file().then_some(system)
}

/// The application icon, carried in the binary so a build-tree install gets the
/// same launcher as a packaged one. Returns the name to use in `Icon=`.
fn install_icon() -> Result<&'static str> {
    const NAME: &str = "be.otterit.kpdrive";
    // A package already put it in the system theme.
    if Path::new("/usr/share/icons/hicolor/scalable/apps").join(format!("{NAME}.svg")).is_file() {
        return Ok(NAME);
    }
    let dir = xdg("XDG_DATA_HOME", ".local/share")?.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{NAME}.svg")), include_str!("../assets/icon.svg"))?;
    Ok(NAME)
}

/// Puts the account window in the application launcher.
pub fn launcher() -> Result<PathBuf> {
    if let Some(system) = packaged("applications/be.otterit.kpdrive.desktop") {
        return Ok(system);
    }
    let dir = xdg("XDG_DATA_HOME", ".local/share")?.join("applications");
    std::fs::create_dir_all(&dir)?;
    let exe = ui_binary()?;
    let icon = install_icon()?;
    let file = dir.join("be.otterit.kpdrive.desktop");
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=Proton Drive\nGenericName=Cloud storage\nGenericName[fr]=Stockage en ligne\nComment=Account, storage and activity log for Proton Drive\nComment[fr]=Compte, stockage et journal d’activité pour Proton Drive\nExec={} %u\nIcon={}\nTerminal=false\nCategories=Utility;FileTools;\nStartupNotify=true\n",
            exe.display(),
            icon
        ),
    )?;
    Ok(file)
}

/// Where the window binary is: next to this one when run from a build tree,
/// otherwise whatever is on PATH.
pub fn ui_binary() -> Result<PathBuf> {
    let sibling = std::env::current_exe()?.with_file_name("kpdrive-ui");
    Ok(if sibling.is_file() { sibling } else { PathBuf::from("kpdrive-ui") })
}

/// Adds "Copy Proton Drive link" to Dolphin's right-click menu. KDE has no way
/// to limit a service menu to one directory, so the entry appears everywhere and
/// the command declines politely outside the sync folder.
pub fn servicemenu() -> Result<PathBuf> {
    if let Some(system) = packaged("kio/servicemenus/kpdrive-share.desktop") {
        return Ok(system);
    }
    let dir = xdg("XDG_DATA_HOME", ".local/share")?.join("kio/servicemenus");
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    let file = dir.join("kpdrive-share.desktop");
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Service\n# all files inherit from application/octet-stream\nMimeType=application/octet-stream;inode/directory;\nActions=kpdriveShare;\nX-KDE-MaxNumberOfUrls=1\n\n[Desktop Action kpdriveShare]\nName=Copy Proton Drive link\nName[fr]=Copier le lien Proton Drive\nIcon=emblem-shared\nExec={} share --copy %f\n",
            exe.display()
        ),
    )?;
    // Service menus are only picked up once the desktop cache is rebuilt.
    let _ = std::process::Command::new("kbuildsycoca6").arg("--noincremental").output();
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_encodes() {
        assert_eq!(href(Path::new("/home/me/Proton Drive")), "file:///home/me/Proton%20Drive");
    }
}

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

/// Adds a "Proton Drive" entry to Dolphin's Places unless one already points at `root`.
pub fn places_entry(root: &Path) -> Result<bool> {
    let file = xdg("XDG_DATA_HOME", ".local/share")?.join("user-places.xbel");
    let xml = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let href = href(root);
    if xml.contains(&format!("href=\"{href}\"")) {
        return Ok(false);
    }
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let entry = format!(
        " <bookmark href=\"{href}\">\n  <title>Proton Drive</title>\n  <info>\n   <metadata owner=\"http://freedesktop.org\">\n    <bookmark:icon name=\"folder-cloud\"/>\n   </metadata>\n   <metadata owner=\"http://www.kde.org\">\n    <ID>{id}/0</ID>\n   </metadata>\n  </info>\n </bookmark>\n</xbel>"
    );
    let updated = xml.replacen("</xbel>", &entry, 1);
    if updated == xml {
        anyhow::bail!("{} has no </xbel> closing tag", file.display());
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
            "[Desktop Entry]\nType=Application\nName=Proton Drive (kpdrive)\nComment=Sync Proton Drive with ~/ProtonDrive\nExec={} sync --watch\nIcon=folder-cloud\nTerminal=false\nX-KDE-autostart-after=panel\n",
            exe.display()
        ),
    )?;
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

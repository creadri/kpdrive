# kpdrive

Simple KDE Proton Drive sync Client designed to have a simple solution.

## Decisions

- **Not a FUSE mount**: only synching folders, no 
- **API client:** our own thin client over the Drive REST API. Crypto comes from
  Proton's official [proton-crypto-rs](https://github.com/ProtonMail/proton-crypto-rs)
  (MIT, pure-Rust `rustpgp` backend, pinned commit). No third-party SDK ports.
- **One binary:** `kpdrive` runs as the daemon or as a CLI talking to it over a Unix socket.
- **KDE integration without C++:** tray via StatusNotifierItem, notifications and
  KWallet via D-Bus, prompts via `kdialog`, browser via `xdg-open`, Places entry via `user-places.xbel`,
  context menu via `kio/servicemenus`.
- **Sharing:** a public link hangs off a share on the node. The share's session
  key is re-wrapped under a bcrypt-derived key from the link password, and the
  visitor proves the password by SRP. The generated password is also stored
  encrypted to our own address key, which is how the link can be shown again.
- **Window:** `ui/` is a separate crate so the CLI and daemon never need Qt.
  It is Rust plus QML (Kirigami) through cxx-qt; network work runs on a worker
  thread and results are posted back to the Qt thread.
- **The one C++ piece:** Dolphin overlay icons (`KOverlayIconPlugin`). Lives in its own
  directory, talks to the daemon socket, optional package.
- **Packaging:** 
  - Manual install: `cargo install`
  - RPM targetting Fedora
  - deb
  - AUR depending on demand
- **Terms:** Proton permits personal, non-commercial third-party use of its SDK/API today.

## Milestones

1. Login via browser sign-in (Proton session fork: the browser handles password, 2FA, captcha), session stored in KWallet.
2. List root, download one file.
3. Remote → local one-way sync driven by the event stream.
4. Local → remote uploads (`put`, `mkdir`); `sync` pushes new/edited local files and trashes locally deleted ones.
5. Tray, Places entry, notifications, autostart, Dolphin overlay plugin.
6. Conflicts: both versions kept.
7. Public links (`share`/`unshare`) and a "Copy Proton Drive link" Dolphin menu entry.
8. Photos: timeline download.
9. Account window and activity log with search and retention (`kpdrive-ui`).

Conflicts, trash, sharing and photos wait until two-way sync is stable.

## Usage

```
kpdrive login                 # browser sign-in, session stored in KWallet
kpdrive status
kpdrive ls [path]
kpdrive get <remote> [local]
kpdrive put <local> [remote-folder]   # new file, or new revision if the name exists
kpdrive mkdir <remote>
kpdrive photos [--dest DIR]   # download the Photos timeline
kpdrive share <path> [--copy] [--password P] [--expires-days N]
kpdrive unshare <path>
kpdrive setup [--root DIR]    # local folder, ignore file, Places entry, autostart, launcher
kpdrive sync [--root DIR] [--watch] [--force]   # two-way Drive <-> local; --watch = daemon with tray
kpdrive logs [--search TERM] [--lines N] [--retention DAYS]
kpdrive logout
```

`kpdrive-ui` opens the account window: who is signed in, storage used, links to
Proton Drive and the account page on the web, sign in / sign out, and a second
tab holding the activity log with a live search box and the retention setting.
It is also in the application launcher and in the tray menu after `kpdrive setup`.

Activity goes to `~/.local/share/kpdrive/logs/YYYY-MM-DD.log` as plain text,
one file per day, pruned to `log_retention_days` from
`~/.config/kpdrive/config.json` (30 by default).

The sync folder is `~/ProtonDrive` until you change it, with `--root` on
`setup` or `sync`, or the **Change…** button in the window. Changing it moves
what is already synced when the two paths are on one filesystem, which keeps
every recorded path valid; across filesystems the old folder is left alone and
the files are fetched again into the new one. The choice lives in
`~/.config/kpdrive/config.json`. A running daemon keeps the old path until it
restarts.

`.protonignore` in the root of the sync folder lists paths to leave alone, with
the same syntax as `.gitignore` (`*`, `**`, a trailing slash for folders only, a
leading slash to anchor, `!` to make an exception); it is matched by the same
library ripgrep uses. Ignored paths are neither uploaded nor downloaded nor
deleted, and they do not wake the daemon. The file itself syncs, as `.gitignore`
does, and `setup` writes a commented starter.

Sync state lives in `~/.local/share/kpdrive/state.json`. Rules:

- Edited on one side only: copied to the other side.
- Edited on **both** sides: both versions are kept. The remote version takes the
  name; the local one becomes `name (conflict copy 2026-09-19 18-30-26).ext` and
  is uploaded as a new file in the same pass. Contents are compared first, so
  two identical files are never split into a copy. Timestamps in those names
  are UTC.
- Deleted locally, unchanged remotely: trashed remotely (restorable in the web app).
- Deleted locally but changed remotely: the remote version is restored, because
  a deletion must not discard someone else's edit.
- Removed remotely: deleted locally only if untouched since we wrote it.
Set `KPDRIVE_DEBUG=1` for event-page diagnostics.

Photos are a separate volume with a flat, capture-time-ordered timeline, so
`photos` is its own command and writes to `~/Pictures/Proton Drive/YYYY/MM/`
(state in `photos.json`). It only downloads. The destination must be outside the
file sync folder, or the file sync would upload the whole library back into
Drive; kpdrive refuses that rather than letting it happen.

`share` returns a public read-only link. The URL ends in `#<password>`: that
fragment never leaves the browser, so the link itself is the credential. Keep
it secret, or add `--password` for a second one the recipient must type, which
is deliberately *not* in the URL and has to be sent separately. Passing `share`
a path that already has a link returns that link unchanged. `--copy` puts it on
the clipboard and is what the Dolphin right-click entry uses; `share` accepts a
local path inside the sync folder as well as a remote one.

The daemon (`sync --watch`) shows a Plasma tray icon, sends desktop
notifications for conflicts and errors, and serves `$XDG_RUNTIME_DIR/kpdrive.sock`
(line protocol: `STATUS <path>` → `OK|SYNC|NONE`, `ROOT`, `SYNC`, `QUIT`).
The Dolphin overlay plugin in `dolphin-overlay/` uses that socket; see its README.

## Build

```
cargo build                 # CLI and daemon
cargo build -p kpdrive-ui   # the window; needs qt6-qtdeclarative-devel and kf6-kirigami
```

## Releases

Pushing a version tag builds packages and publishes them as a GitHub release.
The tag is the single source of truth for the version, and the workflow refuses
to build if the crates disagree with it:

```
# bump both crates first, then
git commit -am "Release 0.2.0"
git tag -a v0.2.0 -m "kpdrive 0.2.0"
git push origin dev v0.2.0
```

A tag containing a hyphen (`v0.2.0-rc1`) is published as a pre-release, and
`.github/workflows/release.yml` can also be run by hand against an existing tag.

| Target | Built in | Packages |
| --- | --- | --- |
| Fedora, current and previous release | `fedora:44`, `fedora:43` | `kpdrive`, `kpdrive-ui`, `kpdrive-dolphin` (plus the source RPM) |
| Debian stable | `debian:trixie` | the same three as `.deb` |

Bump the Fedora numbers in both workflows when Fedora moves on; every filename
carries its dist tag, so which is which is never in doubt. The Debian build
installs Rust through rustup rather than using the distro package, because
Debian stable ships 1.85 while some dependencies want 1.92.

`kpdrive` holds the CLI and the daemon, `kpdrive-ui` the window, and
`kpdrive-dolphin` the overlay icons and the context-menu entry. Installing them
does *not* start syncing on its own; `kpdrive setup` is still how you choose a
folder and opt into autostart.

Every push and pull request runs the build, the tests, the Dolphin plugin and a
spec parse check on both Fedora releases, plus a fast validation of the Debian
control and changelog files.

To build packages locally:

```
rpmbuild -ba --define "kpdrive_version 0.1.0" packaging/kpdrive.spec  # needs a matching tarball in ~/rpmbuild/SOURCES
dpkg-buildpackage -b -us -uc                                          # on Debian
```

## Icon

`assets/icon.svg` is the application icon: pure vector, under a kilobyte, with
the letter drawn as a path so it does not depend on an installed font. It reads
down to 16px and looks the same against light and dark backgrounds. Packages
install it as `be.otterit.kpdrive` in the hicolor theme, and `kpdrive setup`
drops a copy under the user's icon directory so a build-tree install gets the
same launcher. `assets/icon-draft.svg` is the earlier sketch it came from.

## Licence

GNU GPL v3 **or later** (`GPL-3.0-or-later`); see [LICENSE](LICENSE).

Every library kpdrive links permits this. Qt 6 here is `LGPL-3.0-only`, which
explicitly allows conveying the result under GPL v3. The KDE Frameworks behind
the Dolphin plugin are `LGPL-2.0-or-later`, and `KOverlayIconPlugin` itself is
`LGPL-2.0-only OR LGPL-3.0-only OR LicenseRef-KDE-Accepted-LGPL`, so the v3
option applies. Three Rust dependencies are Apache-2.0 with no alternative
(`sync_wrapper` at run time, `clang-sys` and `codespan-reporting` at build
time); Apache-2.0 is compatible with GPL v3 and *not* with GPL v2, which is what
ruled v2 out. Everything else is MIT, BSD, Unicode or dual MIT/Apache.

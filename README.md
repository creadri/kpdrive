# kpdrive

Proton Drive sync client for KDE Plasma, written in Rust.

## Decisions

- **Shape:** sync folder (default `~/ProtonDrive`), Nextcloud-style. Not a FUSE mount.
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
- **The one C++ piece:** Dolphin overlay icons (`KOverlayIconPlugin`). Lives in its own
  directory, talks to the daemon socket, optional package.
- **Packaging:** `cargo install` now; RPM, deb, AUR later.
- **Terms:** Proton permits personal, non-commercial third-party use of its SDK/API today.

## Milestones

1. Login via browser sign-in (Proton session fork: the browser handles password, 2FA, captcha), session stored in KWallet.
2. List root, download one file.
3. Remote → local one-way sync driven by the event stream.
4. Local → remote uploads (`put`, `mkdir`); `sync` pushes new/edited local files and trashes locally deleted ones.
5. Tray, Places entry, notifications, autostart, Dolphin overlay plugin.
6. Conflicts: both versions kept.
7. Public links (`share`/`unshare`) and a "Copy Proton Drive link" Dolphin menu entry.

Conflicts, trash, sharing and photos wait until two-way sync is stable.

## Usage

```
kpdrive login                 # browser sign-in, session stored in KWallet
kpdrive status
kpdrive ls [path]
kpdrive get <remote> [local]
kpdrive put <local> [remote-folder]   # new file, or new revision if the name exists
kpdrive mkdir <remote>
kpdrive share <path> [--copy] [--password P] [--expires-days N]
kpdrive unshare <path>
kpdrive setup [--root DIR]    # local folder, Dolphin Places entry, autostart
kpdrive sync [--root DIR] [--watch] [--force]   # two-way Drive <-> local; --watch = daemon with tray
kpdrive logout
```

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
cargo build
```

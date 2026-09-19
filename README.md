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
- **The one C++ piece:** Dolphin overlay icons (`KOverlayIconPlugin`). Lives in its own
  directory, talks to the daemon socket, optional package.
- **Packaging:** `cargo install` now; RPM, deb, AUR later.
- **Terms:** Proton permits personal, non-commercial third-party use of its SDK/API today.

## Milestones

1. Login via browser sign-in (Proton session fork: the browser handles password, 2FA, captcha), session stored in KWallet.
2. List root, download one file.
3. Remote → local one-way sync driven by the event stream.
4. Local → remote uploads (`put`, `mkdir`); `sync` pushes new/edited local files and trashes locally deleted ones.
5. Tray, Places entry, notifications, autostart, Dolphin overlay plugin (servicemenus skipped: no action needs one yet).

Conflicts, trash, sharing and photos wait until two-way sync is stable.

## Usage

```
kpdrive login                 # browser sign-in, session stored in KWallet
kpdrive status
kpdrive ls [path]
kpdrive get <remote> [local]
kpdrive put <local> [remote-folder]   # new file, or new revision if the name exists
kpdrive mkdir <remote>
kpdrive setup [--root DIR]    # local folder, Dolphin Places entry, autostart
kpdrive sync [--root DIR] [--watch] [--force]   # two-way Drive <-> local; --watch = daemon with tray
kpdrive logout
```

Sync state lives in `~/.local/share/kpdrive/state.json`. Rules: a file edited
locally is never overwritten (if the remote changed too, it is reported and
neither side is pushed); a file deleted locally and unchanged remotely is
trashed remotely; a file removed remotely is deleted locally only if untouched.
Set `KPDRIVE_DEBUG=1` for event-page diagnostics.

The daemon (`sync --watch`) shows a Plasma tray icon, sends desktop
notifications for conflicts and errors, and serves `$XDG_RUNTIME_DIR/kpdrive.sock`
(line protocol: `STATUS <path>` → `OK|SYNC|NONE`, `ROOT`, `SYNC`, `QUIT`).
The Dolphin overlay plugin in `dolphin-overlay/` uses that socket; see its README.

## Build

```
cargo build
```
